//! The town page's side of the App: it starts and stops the link worker, hands it the words,
//! and moves sync messages between the worker and the model. The UI thread never waits on the
//! worker; a full queue keeps its commands for the next frame.

use makepad_town_core::key::PublicKey;
use crate::App;
use makepad_town_core::hub::{Attempt, Carrier};
use std::collections::{BTreeMap, BTreeSet};
use makepad_create::link_worker::{Known, ConnectView, DeviceLink, FromWorker, LinkWorker, PairWords, Status, ToWorker};
use makepad_create::model::{wall_ms, Action, AddComputer, Model, NoticeAction, Origin, PairSide, ShareState, TownNotice};
use makepad_town_core::event::Id;
use makepad_create::{consent, pair_failure, ADDRESS_FAILED, DECLINED, PAIR_FAILED};
use makepad_create::lists::{HomeAction, Page, Pane};
use makepad_town_core::queue::Credential;
use makepad_create::view::{tool_command, MemberId, MemberSeat, Presence, Seat};
use makepad_widgets::*;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HomeStep {
    #[default] Overview,
    Ai(Id),
    Pair(PairSide),
    AddMenu(Id),
    Card { town: Id, member: MemberId },
}

pub(super) struct Removing {
    pub(super) town: Id,
    member: MemberId,
    target: PublicKey,
    name: String,
    pub(super) timer: Timer,
}

fn removable_device(model: &Model, member: MemberId) -> Option<PublicKey> {
    let key = match member {
        MemberId::Computer(key) => key,
        MemberId::You => model.me(),
        MemberId::YourAi | MemberId::Agent(_) => return None,
    };
    model.can_remove_device(&key).then_some(key)
}

fn card_pressed<T: ActionTrait + PartialEq>(button: ButtonRef, actions: &Actions, shown: &T) -> bool {
    actions.iter().filter_map(|a| a.as_widget_action()).any(|a| a.widget_uid == button.widget_uid()
        && matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))
        && a.data.as_ref().and_then(|d| d.downcast_ref::<T>()) == Some(shown))
}

fn can_connect(model: &Model, link: &ConnectView, carriers: &Carriers, seat: &MemberSeat) -> bool {
    matches!(seat.id, MemberId::Computer(key) if seat.presence == Presence::Away && !link.first_contact
        && model.state.active_member(&model.me()) && model.state.active_member(&key) && !model.linked_with(&key)
        && model.peer_update(&key).is_none() && !carriers.wants_device(&key)
        && matches!(link.device_link(&key, 0), DeviceLink::Offline { .. })
        && carriers.revision != Some((model.me(), model.wanted_revision()))
        && model.known_device(&key).is_some() && model.wanted().1.contains(&(key, model.open_id())))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HookFailure { Expired, MissingRemote, Refused }

impl HookFailure {
    fn line(self) -> &'static str {
        match self {
            Self::Expired => "That sentence has expired.",
            Self::MissingRemote => "Town couldn't find makepad-remote.",
            Self::Refused => "Town couldn't let your AI join.",
        }
    }
}

pub(super) enum HookPhase {
    Waiting { code: Credential, sentence: String, preview: String, revealed: bool, expires: u64 },
    Joined { binding: Credential, agent: Id },
    Connected(Id),
    Failed(HookFailure),
    Stopped,
}

impl HookPhase {
    fn clipboard_text(&self) -> Option<&str> {
        match self { Self::Waiting { sentence, .. } => Some(sentence), _ => None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ShownHook { town: Id, generation: u64 }

pub(super) struct HookStep {
    generation: u64,
    outstanding: Option<Credential>,
    target: Option<Id>,
    pub(super) phase: HookPhase,
}

#[derive(Default)]
pub(super) struct Carriers {
    revision: Option<(PublicKey, u64)>,
    wanted: Vec<Known>,
    wanted_devices: BTreeSet<PublicKey>,
    towns: BTreeSet<(PublicKey, Id)>,
    up: BTreeMap<Attempt, String>,
    words_request: u64,
    words: Option<Attempt>,
    writable: BTreeSet<Attempt>,
    catch_up_timer: Option<(std::time::Instant, Timer)>,
    calls: BTreeMap<PublicKey, Attempt>,
}

impl Carriers {
    fn wants_device(&self, key: &PublicKey) -> bool {
        self.wanted_devices.contains(key)
    }
}

impl App {
    fn consumed_sync(&mut self, at: Attempt) {
        // Flush after this UI turn. A fast worker cannot refill this attempt
        // while the UI is still draining the other carriers' notices.
        if let Some((None, ToWorker::SyncRead(batch))) = self.link_queue.back_mut() {
            batch.push(at);
        } else {
            self.link_queue.push_back((None, ToWorker::SyncRead(vec![at])));
        }
    }

    pub(super) fn town_notice_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(action) = TownNotice::clicked(actions) else { return };
        let Some(model) = &mut self.model else { return };
        match model.act_on_notice(action) {
            Ok(false) => return,
            Ok(true) => self.view.share_error = None,
            Err(error) => self.view.share_error = Some(error),
        }
        if self.view.share_error.is_some() || matches!(action, NoticeAction::Offer { .. }) {
            self.view.page = Page::Home;
            self.view.show_pane(Pane::Page);
            self.update_page(cx);
        }
        self.update_presence(cx);
        self.update_town_notice(cx);
        self.update_connect(cx);
    }

    pub(super) fn connect_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let town = self.model.as_ref().map(Model::open_id);
        let tree_join = actions.iter().filter_map(|a| a.as_widget_action()).any(|click|
            matches!(click.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))
                && click.data.as_ref().and_then(|d| d.downcast_ref::<HomeAction>()) == Some(&HomeAction::Join));
        if tree_join {
            self.open_pairing(cx, PairSide::Join);
            return
        }
        // "Add AI" under a chat line that no AI heard opens the town page's inline step.
        let chat_add_ai = actions.iter().filter_map(|a| a.as_widget_action()).any(|click|
            matches!(click.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))
                && matches!(click.data.as_ref().and_then(|d| d.downcast_ref::<HomeAction>()), Some(HomeAction::AddAi(id)) if Some(*id) == town));
        if chat_add_ai && self.view.page != Page::Home {
            self.view.page = Page::Home;
            self.view.show_pane(Pane::Page);
            self.update_page(cx);
        }
        if !matches!(self.view.page, Page::Home | Page::Join) { return }
        for click in actions.iter().filter_map(|a| a.as_widget_action()) {
            if !matches!(click.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { continue }
            let Some(action) = click.data.as_ref().and_then(|data| data.downcast_ref::<HomeAction>()) else { continue };
            match *action {
                HomeAction::AddMember(id) if Some(id) == town && self.view.workspace.writable => {
                    self.home_step = if self.home_step == HomeStep::AddMenu(id) { self.pending_home_step(id) } else { HomeStep::AddMenu(id) };
                }
                HomeAction::AddAi(id) if Some(id) == town && self.view.workspace.writable => {
                    self.home_step = HomeStep::Ai(id);
                    if !matches!(self.hooks.get(&id).map(|s| &s.phase), Some(HookPhase::Waiting { .. } | HookPhase::Joined { .. })) {
                        self.prepare_hook(cx, self.away_ai());
                    }
                }
                HomeAction::Pair(id, choice) if Some(id) == town && self.home_step == HomeStep::AddMenu(id)
                    && self.view.workspace.writable && self.pair_problem(PairSide::Add(id, choice)).is_none() => self.open_pairing(cx, PairSide::Add(id, choice)),
                HomeAction::Card { town: id, member } if Some(id) == town && self.view.members.seats.iter().any(|s| s.id == member) => {
                    let closing = matches!(self.home_step, HomeStep::Card { member: open, .. } if open == member);
                    self.home_step = if closing { self.pending_home_step(id) } else { HomeStep::Card { town: id, member } };
                    if !closing && member == MemberId::You {
                        self.hub_probe(cx);
                        match self.model.as_ref().unwrap().home.computer_label() {
                            Ok(label) => self.ui.text_input(cx, ids!(computer_label)).set_text(cx, &label),
                            Err(err) => self.view.share_error = Some(err.to_string()),
                        }
                    }
                }
                _ => continue,
            }
            self.update_connect(cx);
            return
        }
        if self.member_card_actions(cx, actions) { self.refresh(cx); return }
        if self.share_actions(cx, actions) { self.refresh(cx); return }
        if let Some(side) = self.pair_side_shown().filter(|_| self.pair_step_visible()) {
            let form = self.pair_form(cx, side);
            if form.button(cx, ids!(pair_close)).clicked(actions) { self.end_pairing(cx); self.update_connect(cx); return }
            if self.model.as_ref().and_then(Model::devices_problem).is_some() {
                if form.button(cx, ids!(connect_devices_retry)).clicked(actions) { let _ = self.retry_devices(cx); }
                if form.button(cx, ids!(connect_devices_forget)).clicked(actions) { let _ = self.review_forget_devices(cx); }
                if form.button(cx, ids!(connect_devices_cancel)).clicked(actions) {
                    self.view.connect.confirm_forget = false;
                    self.update_devices(cx);
                }
                return
            }
            let input = form.text_input(cx, ids!(connect_peer_words));
            if form.button(cx, ids!(pair_copy)).clicked(actions) {
                if let Some(words) = &self.view.connect.own_words { cx.copy_to_clipboard(words); }
            }
            if form.button(cx, ids!(pair_retry)).clicked(actions) && self.view.connect.failed.is_some() && self.pair_problem(side).is_none() {
                let renewed = self.view.connect.own_words.is_some() || self.view.connect.renewed;
                self.start_words(cx, side, renewed);
                self.update_connect(cx);
            } else if input.changed(actions).is_some() {
                let draft = input.text();
                self.view.connect.edit_words(&draft);
                self.try_pair_draft(cx, &draft);
                self.link_event(cx);
                self.update_connect(cx);
            }
            return
        }
        if let HomeStep::Ai(id) = self.home_step {
            if Some(id) != town { return }
            let shown = self.hooks.get(&id).map(|s| ShownHook { town: id, generation: s.generation });
            let current_press = |path: &[LiveId]| {
                let button = self.ui.button(cx, path);
                button.clicked(actions) && actions.iter().filter_map(|a| a.as_widget_action()).any(|click|
                    click.widget_uid == button.widget_uid() && shown.is_some()
                        && click.data.as_ref().and_then(|d| d.downcast_ref::<ShownHook>()).copied() == shown)
            };
            if current_press(ids!(ai_close)) {
                if self.stop_hook(id) { self.home_step = self.pending_home_step(id); }
            } else if current_press(ids!(ai_retry)) {
                self.prepare_hook(cx, self.hooks.get(&id).and_then(|step| step.target));
            } else if current_press(ids!(ai_show)) {
                let Some(HookPhase::Waiting { revealed, .. }) = self.hooks.get_mut(&id).map(|s| &mut s.phase) else { return };
                if !consent::granted(cx, self.call.consent_home.verified()) {
                    if let Some(by) = cx.injected_by() { log!("town: join code toggle refused for {}", by.replace(['\r', '\n'], " ")); }
                    return
                }
                *revealed = !*revealed;
            } else if current_press(ids!(ai_copy)) {
                if let Some(sentence) = self.hooks.get(&id).and_then(|s| s.phase.clipboard_text()) { cx.copy_to_clipboard(sentence); }
            } else { return }
            self.update_connect(cx);
        }
    }

    pub(super) fn pending_home_step(&mut self, town: Id) -> HomeStep {
        if self.draft_town != Some(town) {
            if let Some(HookPhase::Waiting { revealed, .. }) = self.hooks.get_mut(&town).map(|s| &mut s.phase) { *revealed = false; }
        }
        if let Some(side) = self.view.connect.side {
            if matches!(side, PairSide::Add(id, _) if id == town) { return HomeStep::Pair(side) }
        }
        if self.hooks.get(&town).is_some_and(|s| matches!(s.phase, HookPhase::Waiting { .. } | HookPhase::Joined { .. } | HookPhase::Failed(_))) {
            return HomeStep::Ai(town)
        }
        HomeStep::Overview
    }

    fn away_ai(&self) -> Option<Id> {
        let model = self.model.as_ref()?;
        let away = |id| self.view.workspace.connect.rows.iter().any(|a| a.id == id && a.owner && a.away);
        model.my_agent(self.next_ai.get(&model.open_id()).copied()).filter(|id| away(*id))
            .or_else(|| self.view.workspace.connect.rows.iter().find(|a| a.owner && a.away).map(|a| a.id))
    }

    fn member_card_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        if let Some(removing) = &self.removing {
            if !card_pressed(self.ui.button(cx, ids!(undo_remove)), actions, &removing.timer.0) { return false }
            self.cancel_removal(cx);
            return true
        }
        let HomeStep::Card { town, member } = self.home_step else { return false };
        let Some(model) = &mut self.model else { return false };
        if model.open_id() != town { return false }
        let Some(seat) = self.view.members.seats.iter().find(|s| s.id == member) else { return false };
        let pressed = |path| card_pressed(self.ui.button(cx, path), actions, &self.home_step);
        if let Some(target) = pressed(ids!(remove_member)).then(|| removable_device(model, member)).flatten() {
            self.removing = Some(Removing {
                town, member, target, name: seat.name.clone(), timer: cx.start_timeout(5.0),
            });
        } else if member == MemberId::You && pressed(ids!(hub_offer)) {
            if !consent::granted(cx, self.call.consent_home.verified()) { return false }
            let on = model.state.provides(&model.me()).is_empty();
            if let Err(why) = model.act(Action::Provide { on }, Origin::Ui) { self.view.share_error = Some(why); }
        } else if member == MemberId::You {
            let field = self.ui.text_input(cx, ids!(computer_label));
            if field.escaped(actions) {
                self.home_step = self.pending_home_step(town);
                return true
            }
            let Some((name, _)) = field.returned(actions) else { return false };
            match model.set_person_name(&name) {
                Ok(()) => self.home_step = self.pending_home_step(town),
                Err(err) => self.view.share_error = Some(err.to_string()),
            }
        } else if let MemberId::Computer(key) = member {
            if !pressed(ids!(device_link)) || !can_connect(model, &self.view.connect, &self.carriers, seat) { return false }
            self.connect_resume(cx, key);
        } else if let MemberId::Agent(agent) = member {
            if !pressed(ids!(remove_ai)) || seat.seat != Seat::YourAi || !self.view.workspace.writable { return false }
            if let Err(why) = model.remove_agent(agent) {
                self.view.share_error = Some(why);
            } else {
                if self.next_ai.get(&town) == Some(&agent) { self.next_ai.remove(&town); }
                if self.hooks.get(&town).is_some_and(|s| s.target == Some(agent)
                    || matches!(s.phase, HookPhase::Connected(a) | HookPhase::Joined { agent: a, .. } if a == agent)) {
                    self.stop_hook(town);
                }
                self.home_step = self.pending_home_step(town);
            }
        } else { return false }
        true
    }

    /// The pairing side whose form the open page holds: Join's own page, or an Add step on Home.
    pub(super) fn pair_side_shown(&self) -> Option<PairSide> {
        match (self.view.page, self.home_step) {
            (Page::Join, _) => Some(PairSide::Join),
            (Page::Home, HomeStep::Pair(side @ PairSide::Add(..))) => Some(side),
            _ => None,
        }
    }

    pub(super) fn pair_step_visible(&self) -> bool {
        self.pair_side_shown().is_some_and(|side| self.view.connect.shows_pairing(side))
    }

    /// Each side has its own form: Join's on its page, Add's on the town's Home.
    pub(super) fn pair_form(&self, cx: &mut Cx, side: PairSide) -> WidgetRef {
        match side {
            PairSide::Join => self.ui.widget(cx, ids!(join_page)),
            PairSide::Add(..) => self.ui.widget(cx, ids!(home_page.pair_step)),
        }
    }

    pub(super) fn switch_town(&mut self, cx: &mut Cx, town: Id) {
        if self.flush_doc(cx).is_err() { return }
        if self.removing.as_ref().is_some_and(|r| r.town != town) { self.cancel_removal(cx); }
        if let Some(model) = &mut self.model {
            if let Err(e) = model.open_town(town) { log!("town: opening a town: {e}"); }
        }
        self.refresh(cx);
    }

    pub(super) fn cancel_removal(&mut self, cx: &mut Cx) {
        let Some(Removing { timer, .. }) = self.removing.take() else { return };
        cx.stop_timer(timer);
    }

    pub(super) fn finish_removal(&mut self, cx: &mut Cx) {
        let Some(Removing { town, member, target, timer, .. }) = self.removing.take() else { return };
        cx.stop_timer(timer);
        if self.view.page != Page::Home || (self.view.narrow() && self.view.pane != Pane::Page) {
            return;
        }
        let Some(model) = &mut self.model else { return };
        if model.open_id() != town || !model.can_remove_device(&target) {
            self.view.share_error = Some("The town changed before the removal could finish.".into());
        } else {
            self.view.share_error = model.remove_device(target, false).err();
        }
        if self.home_step == (HomeStep::Card { town, member }) {
            self.home_step = self.pending_home_step(town);
        }
    }

    fn share_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        let Some(model) = &mut self.model else { return false };
        if !self.ui.button(cx, ids!(share_cancel)).clicked(actions) { return false }
        model.cancel_offer();
        self.view.share_error = None;
        true
    }

    fn update_sharing(&self, cx: &mut Cx) {
        let Some(model) = &self.model else { return };
        let state = model.share_state();
        let show = |cx: &mut Cx, path, yes| self.ui.widget(cx, path).set_visible(cx, yes);
        let line = |cx: &mut Cx, path, text: &str| self.ui.label(cx, path).set_text(cx, text);
        let note = self.view.share_error.as_deref().or(model.share_note());
        let offered = matches!(state, ShareState::Offered(_));
        show(cx, ids!(share_note), note.is_some() || offered);
        line(cx, ids!(share_note), note.unwrap_or("Waiting for the other computer to answer."));
        show(cx, ids!(share_cancel), offered);
    }

    pub(super) fn update_devices(&self, cx: &mut Cx) {
        let Some(side) = self.pair_side_shown() else { return };
        let form = self.pair_form(cx, side);
        let problem = self.model.as_ref().and_then(Model::devices_problem);
        form.widget(cx, ids!(connect_devices)).set_visible(cx, problem.is_some());
        // An unreadable pin list must be recovered before another pairing.
        form.widget(cx, ids!(connect_form)).set_visible(cx, problem.is_none());
        if let Some(reason) = problem {
            form.label(cx, ids!(connect_devices_detail)).set_text(cx, self.view.connect.devices_error.as_deref().unwrap_or(reason));
            let confirm = self.view.connect.confirm_forget;
            form.widget(cx, ids!(connect_devices_actions)).set_visible(cx, !confirm);
            form.widget(cx, ids!(connect_devices_confirmation)).set_visible(cx, confirm);
        }
    }

    pub(super) fn retry_devices(&mut self, cx: &mut Cx) -> Result<(), String> {
        let result = self.model.as_mut().ok_or("Town is not ready.")?.devices_retry();
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
        if !self.pair_step_visible() { self.open_pairing(cx, PairSide::Join); }
        if !self.pair_step_visible() { return Err("Open the pairing step to recover known devices.".into()) }
        self.view.connect.confirm_forget = true;
        self.view.connect.devices_error = None;
        self.update_page(cx);
        Ok(())
    }

    pub(super) fn confirm_forget_devices(&mut self, cx: &mut Cx) {
        if !self.pair_step_visible() || !self.view.connect.confirm_forget { return }
        let Some(model) = &mut self.model else { return };
        let result = model.devices_forget();
        self.finish_devices_action(cx, &result);
    }

    fn finish_devices_action(&mut self, cx: &mut Cx, result: &Result<(), String>) {
        self.view.connect.confirm_forget = false;
        self.view.connect.devices_error = result.as_ref().err().cloned();
        if result.is_ok() {
            if let Some(side) = self.pair_side_shown() { self.view.connect.side = None; self.open_pairing(cx, side); }
        }
        self.read_workspace();
        self.update_presence(cx);
        self.update_connect(cx);
    }

    fn stop_hook(&mut self, town: Id) -> bool {
        let Some(step) = self.hooks.get_mut(&town) else { return true };
        if let Some(code) = step.outstanding {
            let Some(model) = &mut self.model else { return false };
            if let Err(why) = model.withdraw_agent_code(code) {
                log!("town: withdrawing AI sentence: {why}");
                return false
            }
        }
        step.outstanding = None;
        step.phase = HookPhase::Stopped;
        true
    }

    fn prepare_hook(&mut self, cx: &mut Cx, target: Option<Id>) {
        use makepad_strict_json::{obj, s};
        let Some(id) = self.model.as_ref().map(Model::open_id) else { return };
        if !self.stop_hook(id) { return }
        let Some(model) = &mut self.model else { return };
        let phase = if let Some(binary) = &self.remote_binary {
            let result = (|| {
                let endpoint = self.endpoint.as_deref().ok_or("Town has no local endpoint.")?;
                let sentence = |value: &str| tool_command(binary, endpoint, false, "town.join", obj(vec![("code", s(value))]))
                    .map(|cmd| format!("Come join my town in Makepad Town and wait for my messages.\nRun {cmd} and follow what it prints."));
                let preview = sentence("••••••")?;
                let bearer = model.agent_code(target)?;
                let expires = wall_ms().saturating_add(makepad_town_core::link::INVITE_LIFETIME_MS);
                let code = Credential::parse(&bearer).ok_or("Invalid new join code.")?;
                match sentence(&bearer) {
                    Ok(sentence) => Ok(HookPhase::Waiting { code, expires, sentence, preview, revealed: false }),
                    Err(why) => { model.withdraw_agent_code(code)?; Err(why) }
                }
            })();
            result.unwrap_or_else(|why: String| { log!("town: preparing AI sentence: {why}"); HookPhase::Failed(HookFailure::Refused) })
        } else {
            log!("town: preparing AI sentence: makepad-remote missing");
            HookPhase::Failed(HookFailure::MissingRemote)
        };
        let outstanding = match &phase { HookPhase::Waiting { code, .. } => Some(*code), _ => None };
        let generation = self.hooks.get(&id).map_or(1, |s| s.generation.wrapping_add(1));
        self.hooks.insert(id, HookStep { generation, target, outstanding, phase });
        self.expire_hooks(cx, wall_ms());
    }

    pub(super) fn expire_hooks(&mut self, cx: &mut Cx, now: u64) {
        cx.stop_timer(std::mem::take(&mut self.hook_timer));
        let mut next = None;
        let mut expired = Vec::new();
        for (&town, step) in &self.hooks {
            if let HookPhase::Waiting { expires, .. } = step.phase {
                if expires <= now {
                    expired.push(town);
                } else {
                    next = Some(next.map_or(expires, |other: u64| other.min(expires)));
                }
            }
        }
        for town in expired {
            self.stop_hook(town);
            if let Some(step) = self.hooks.get_mut(&town) { step.phase = HookPhase::Failed(HookFailure::Expired); }
        }
        if let Some(end) = next { self.hook_timer = cx.start_timeout((end - now) as f64 / 1000.); }
        self.update_ai_commands(cx);
    }

    pub(super) fn join_hook(&mut self, cx: &mut Cx, code: Credential) -> Result<String, String> {
        let now = wall_ms();
        let waiting = match self.hooks.get(&code.town).map(|s| &s.phase) {
            Some(HookPhase::Waiting { code: issued, expires, .. }) if *issued == code => {
                if now >= *expires {
                    self.expire_hooks(cx, now);
                    return Err("That sentence has expired.".into())
                }
                true
            }
            _ => false,
        };
        let joined = self.model.as_mut().ok_or("Town is not ready.")?.agent_join_with_id(code, waiting);
        if let Some(step) = self.hooks.get_mut(&code.town).filter(|s| s.outstanding == Some(code)) {
            match &joined {
                Ok((bearer, agent)) => {
                    if let Some(binding) = Credential::parse(bearer) {
                        step.outstanding = None;
                        step.phase = HookPhase::Joined { binding, agent: *agent };
                    }
                }
                Err(why) => {
                    let failure = if matches!(why.as_str(), "Unknown or spent join code." | "That sentence has expired.") { HookFailure::Expired } else { HookFailure::Refused };
                    step.phase = HookPhase::Failed(failure);
                }
            }
        }
        if let Err(why) = &joined { log!("town: AI join refused: {why}"); }
        joined.map(|(bearer, _)| bearer)
    }

    pub(super) fn update_ai_commands(&self, cx: &mut Cx) {
        let HomeStep::Ai(id) = self.home_step else { return };
        let Some(step) = self.hooks.get(&id) else { return };
        let shown = ShownHook { town: id, generation: step.generation };
        for path in [ids!(ai_show), ids!(ai_copy), ids!(ai_close), ids!(ai_retry)] {
            self.ui.button(cx, path).set_action_data(shown);
        }
        let waiting = matches!(step.phase, HookPhase::Waiting { .. });
        self.ui.widget(cx, ids!(ai_commands)).set_visible(cx, waiting);
        self.ui.button(cx, ids!(ai_copy)).set_visible(cx, waiting);
        let mut show = self.ui.button(cx, ids!(ai_show));
        show.set_visible(cx, waiting);
        let preview = self.ui.label(cx, ids!(ai_preview));
        if let HookPhase::Waiting { sentence, preview: masked, revealed, .. } = &step.phase {
            let (text, name) = if *revealed { ("Hide", "Hide the join code") } else { ("Show", "Show the join code") };
            if show.text() != text {
                show.set_text(cx, text);
                let name = cx.with_vm(|vm| vm.new_string(name));
                script_apply_eval!(cx, show, {a11y_name: #(name)});
            }
            let sentence = if *revealed { sentence } else { masked };
            preview.set_text(cx, &format!("“{sentence}”"));
        } else {
            preview.set_text(cx, "");
        }
        preview.set_visible(cx, waiting);
        self.ui.button(cx, ids!(ai_retry)).set_visible(cx, matches!(step.phase, HookPhase::Failed(_)));
        let line = match &step.phase {
            HookPhase::Joined { .. } => "Waiting for your AI to connect…",
            HookPhase::Failed(why) => why.line(),
            _ => "",
        };
        self.ui.label(cx, ids!(ai_status)).set_visible(cx, !line.is_empty());
        self.ui.label(cx, ids!(ai_status)).set_text(cx, line);
    }

    pub(super) fn update_connect(&mut self, cx: &mut Cx) {
        self.update_sharing(cx);
        let v = &self.view.connect;
        let town = self.model.as_ref().map(Model::open_id);
        let prompt = self.model.as_ref().and_then(Model::offer_prompt);
        self.ui.widget(cx, ids!(share_incoming)).set_visible(cx, prompt.is_some());
        if let (Some(prompt), Some(model)) = (prompt, &self.model) {
            self.ui.label(cx, ids!(offer_line)).set_text(cx, model.offer_prompt_line());
            for (path, yes) in [(ids!(share_join), true), (ids!(share_decline), false)] {
                self.ui.widget(cx, path).set_action_data(NoticeAction::Answer { shown: prompt, yes });
            }
        }
        let pair = self.pair_step_visible();
        self.ui.widget(cx, ids!(home_page.pair_step)).set_visible(cx, pair && self.view.page == Page::Home);
        let menu = matches!(self.home_step, HomeStep::AddMenu(id) if Some(id) == town);
        self.ui.widget(cx, ids!(add_menu)).set_visible(cx, menu);
        let members = self.ui.flat_list(cx, ids!(home_members.list));
        if let Some(add) = members.item(cx, id!(member_tip), id!(Add)) {
            if let Some(mut tip) = add.borrow_mut::<tip::Tip>() {
                let hint = if menu { "" } else { self.view.pair_problem.as_deref().unwrap_or("Add a member") };
                if tip.text != hint {
                    if menu { cx.widget_action(tip.widget_uid(), tip::TipAction::HoverOut); }
                    hint.clone_into(&mut tip.text);
                }
            }
        }
        if let Some(id) = town.filter(|_| menu) {
            for (path, choice) in [(ids!(home_pair), AddComputer::SomeoneElse), (ids!(home_pair_mine), AddComputer::Mine)] {
                let button = self.ui.button(cx, path);
                button.set_visible(cx, self.view.pair_problem.is_none());
                button.set_enabled(cx, v.can_open_pairing(PairSide::Add(id, choice)));
                button.set_action_data(HomeAction::Pair(id, choice));
            }
            let ai = self.ui.button(cx, ids!(home_ai));
            ai.set_text(cx, if self.away_ai().is_some() { "Bring my AI back" } else { "Add my AI" });
            ai.set_action_data(HomeAction::AddAi(id));
        }
        if let Some(side) = v.side.filter(|_| pair) {
            self.update_devices(cx);
            let town = match side { PairSide::Add(id, _) => self.town_name(id), PairSide::Join => None };
            let form = self.pair_form(cx, side);
            v.show_form(cx, &form, town, self.pair_problem(side).as_deref(), wall_ms() / 1000);
        }
        let ai = matches!(self.home_step, HomeStep::Ai(id) if Some(id) == town);
        self.ui.widget(cx, ids!(ai_section)).set_visible(cx, ai);
        if ai { self.update_ai_commands(cx); } else { self.ui.label(cx, ids!(ai_preview)).set_text(cx, ""); }
        self.update_member_card(cx);
        self.ui.widget(cx, ids!(home_page)).redraw(cx);
    }

    pub(super) fn update_member_card(&mut self, cx: &mut Cx) {
        let card = self.ui.widget(cx, ids!(member_card));
        let town = self.model.as_ref().map(Model::open_id);
        let removing = self.removing.as_ref().filter(|r| Some(r.town) == town);
        let member = match (removing, self.home_step) {
            (Some(r), _) => Some(r.member),
            (None, HomeStep::Card { town: shown, member }) if Some(shown) == town => Some(member),
            _ => None,
        };
        let seat = member.and_then(|id| self.view.members.seats.iter().find(|s| s.id == id));
        let selected = removing.map(|r| r.member).or_else(|| seat.map(|s| s.id));
        if self.view.selected_member != selected {
            self.view.selected_member = selected;
            self.ui.widget(cx, ids!(home_members)).redraw(cx);
        }
        card.set_visible(cx, selected.is_some());
        if removing.is_some() || selected != Some(MemberId::You) {
            let field = card.text_input(cx, ids!(computer_label));
            field.set_visible(cx, false);
            let area = field.area();
            if !area.is_empty() && cx.has_key_focus(area) {
                cx.set_key_focus(Area::Empty);
            }
        }
        let (Some(model), Some(_)) = (&self.model, selected) else { return };
        let undo = card.button(cx, ids!(undo_remove));
        let line = card.label(cx, ids!(removing_line));
        line.set_visible(cx, removing.is_some());
        undo.set_visible(cx, removing.is_some());
        if let Some(removing) = removing {
            for path in [ids!(member_name), ids!(computer_label), ids!(member_status), ids!(hub_model),
                         ids!(device_link), ids!(hub_offer), ids!(remove_ai), ids!(remove_member)] {
                card.widget(cx, path).set_visible(cx, false);
            }
            if removing.target == model.me() {
                line.set_text(cx, "Leaving this town — this device can't rejoin.");
            } else {
                line.set_text(cx, &format!("Removing {} — that device can't rejoin this town.", removing.name));
            }
            undo.set_action_data(removing.timer.0);
            return
        }
        let Some(seat) = seat else { return };
        let you = seat.id == MemberId::You;
        let name = card.label(cx, ids!(member_name));
        name.set_visible(cx, !you);
        let agent = match seat.id { MemberId::Agent(id) => model.state.agents.get(&id), _ => None };
        if let Some(agent) = agent { name.set_text(cx, &format!("{} · {}", seat.name, agent.name)); }
        else { name.set_text(cx, &seat.name); }
        let status = card.label(cx, ids!(member_status));
        let mut detail = seat.status.clone();
        if let MemberId::Computer(key) = seat.id {
            use makepad_town_core::event::Service;
            let services = model.state.provides(&key);
            let mut names: Vec<_> = [(Service::Summary, "summaries"), (Service::Tasks, "task updates"),
                (Service::Transcription, "transcription")].into_iter()
                .filter_map(|(s, word)| services.contains(&s).then_some(word)).collect();
            if let Some(last) = names.pop() {
                detail.push_str(" · offers ");
                if !names.is_empty() {
                    detail.push_str(&names.join(", "));
                    detail.push_str(" and ");
                }
                detail.push_str(last);
            }
        }
        status.set_text(cx, &detail);
        status.set_visible(cx, !detail.is_empty());
        card.text_input(cx, ids!(computer_label)).set_visible(cx, you);
        let hub = card.label(cx, ids!(hub_model));
        hub.set_visible(cx, you);
        let offer = card.button(cx, ids!(hub_offer));
        offer.set_visible(cx, you);
        if you {
            let line = match &self.local_text_model {
                Some(Ok(name)) => format!("AI Hub: {name}"),
                Some(Err(reason)) => reason.clone(),
                None => "Checking AI Hub…".into(),
            };
            hub.set_text(cx, &line);
            offer.set_enabled(cx, self.view.workspace.writable);
            offer.set_text(cx, if model.state.provides(&model.me()).is_empty() {
                "Offer my AI Hub to this town"
            } else { "Stop offering my AI Hub" });
        }
        card.button(cx, ids!(device_link)).set_visible(cx, can_connect(model, &self.view.connect, &self.carriers, seat));
        card.button(cx, ids!(remove_ai)).set_visible(cx, matches!(seat.id, MemberId::Agent(_)) && seat.seat == Seat::YourAi && self.view.workspace.writable);
        let remove = card.button(cx, ids!(remove_member));
        remove.set_visible(cx, removable_device(model, seat.id).is_some());
        remove.set_text(cx, if you { "Leave" } else { "Remove" });
        for path in [ids!(device_link), ids!(hub_offer), ids!(remove_ai), ids!(remove_member)] {
            card.button(cx, path).set_action_data(self.home_step);
        }
    }

    fn town_name(&self, town: Id) -> Option<&str> {
        let tree = self.view.tree.snapshot();
        if tree.town == Some(town) { return tree.name.as_deref() }
        self.model.as_ref()?.town_name(town)
    }

    fn pair_problem(&self, side: PairSide) -> Option<String> {
        match side {
            PairSide::Add(town, _) => self.model.as_ref()?.pair_problem(town),
            PairSide::Join => None,
        }
    }

    /// Both entry actions enter the same form; its target is the captured town id.
    pub(super) fn open_pairing(&mut self, cx: &mut Cx, side: PairSide) {
        if self.model.is_none() || !self.view.connect.can_open_pairing(side) { return }
        if self.flush_doc(cx).is_err() { return }
        match side {
            PairSide::Join => self.view.page = Page::Join,
            PairSide::Add(..) => { self.view.page = Page::Home; self.home_step = HomeStep::Pair(side); }
        }
        self.view.show_pane(Pane::Page);
        if self.view.connect.first_contact && self.view.connect.side == Some(side) {
            self.update_page(cx);
            return
        }
        self.pair_form(cx, side).text_input(cx, ids!(connect_peer_words)).set_text(cx, "");
        if self.pair_problem(side).is_some() || self.model.as_ref().and_then(Model::devices_problem).is_some() {
            self.view.connect.side = Some(side);
            self.view.connect.first_contact = false;
        } else {
            self.start_words(cx, side, false);
        }
        self.update_page(cx);
    }

    fn start_words(&mut self, cx: &mut Cx, side: PairSide, renewed: bool) {
        if let Some(model) = &mut self.model { model.cancel_pair_intent(); }
        self.carriers.words = None;
        self.view.share_error = None;
        cx.stop_timer(std::mem::take(&mut self.pair_timer));
        self.view.connect = ConnectView {
            devices: std::mem::take(&mut self.view.connect.devices),
            open: true, side: Some(side), first_contact: true, renewed, ..ConnectView::default()
        };
        let draft = self.pair_form(cx, side).text_input(cx, ids!(connect_peer_words)).text();
        self.view.connect.edit_words(&draft);
        self.carriers.words_request = self.carriers.words_request.wrapping_add(1);
        let request = self.carriers.words_request;
        let result = self.ensure_worker().and_then(|worker| worker.open_words(cx.task_pool(), request, true));
        match result {
            Ok(open) => self.link_queue.push_back((None, open)),
            Err(e) => {
                log!("town: starting pairing: {e}");
                self.view.connect.open = false;
                self.view.connect.failed = Some(ADDRESS_FAILED.into());
            }
        }
        self.update_presence(cx);
    }

    fn end_pairing(&mut self, cx: &mut Cx) {
        self.home_step = HomeStep::Overview;
        if self.view.page == Page::Join { self.view.page = Page::Home; }
        if self.view.connect.first_contact {
            self.connect_stop(cx);
            self.reconnect_start(cx);
        } else { self.view.connect.side = None; }
    }

    fn try_pair_draft(&mut self, cx: &mut Cx, text: &str) {
        if text.trim().is_empty() || self.view.connect.bad_words.is_some()
            || self.view.connect.failed.is_some() || !self.view.connect.first_contact { return }
        let view = &mut self.view.connect;
        let (Some(model), Some(side), Some(own)) = (&mut self.model, view.side, view.own_words.as_deref()) else { return };
        if view.waiting_since.is_some() { return }
        let now = wall_ms() / 1000;
        let command = PairWords::read(own, text).and_then(|words| model.pair(side, words, now));
        match command {
            Ok(command) => {
                view.bad_words = None;
                view.waiting_since = Some(now);
                self.pair_timer = cx.start_interval(1.);
                self.link_queue.push_back((self.carriers.words, command));
            }
            Err(why) => {
                log!("town: pairing: {why}");
                view.bad_words = Some(pair_failure(&why).into());
            }
        }
    }

    pub(super) fn pair_tick(&self, cx: &mut Cx) {
        let v = &self.view.connect;
        if let Some(since) = v.waiting_since {
            if let Some(side) = self.pair_side_shown().filter(|_| v.blocked.is_none() && self.pair_step_visible()) {
                self.pair_form(cx, side).label(cx, ids!(pair_status)).set_text(cx, &ConnectView::wait_line(since, wall_ms() / 1000));
            }
        }
    }

    fn ensure_worker(&mut self) -> Result<&LinkWorker, String> {
        if self.link.is_none() {
            let model = self.model.as_ref().ok_or("Town is not ready.")?;
            self.link = Some(LinkWorker::new(model.home.clone(), model.device_key(), SignalToUI::set_ui_signal)
                .map_err(|e| e.to_string())?);
        }
        Ok(self.link.as_ref().unwrap())
    }

    /// Publish a complete snapshot. Final messages and the tag withdrawal travel
    /// in one command, including when the bounded worker mailbox is full.
    fn reconcile_carriers(&mut self) {
        let Some(model) = &mut self.model else { return };
        let revision = (model.me(), model.wanted_revision());
        if self.carriers.revision == Some(revision) { return }
        let (wanted, towns) = model.wanted();
        let force = self.carriers.revision.is_none_or(|(me, _)| me != model.me());
        self.carriers.revision = Some(revision);
        if !force && self.carriers.wanted == wanted && self.carriers.towns == towns { return }
        let wanted_devices: BTreeSet<_> = wanted.iter().map(|d| d.pin.device).collect();
        for &(peer, town) in towns.difference(&self.carriers.towns) {
            if self.carriers.up.keys().any(|a| a.carrier == Carrier::Peer(peer)) {
                if let Err(why) = model.start_serving(peer, town) {
                    log!("town: starting sync for a shared town: {why}");
                }
            }
        }
        let mut final_sync = Vec::new();
        let mut retiring = BTreeSet::new();
        for &(peer, town) in self.carriers.towns.difference(&towns) {
            let Some(&at) = self.carriers.up.keys().find(|at| at.carrier == Carrier::Peer(peer)) else { continue };
            match model.finish_carrier_town(peer, town) {
                Ok(batch) => {
                    if !batch.is_empty() { retiring.insert((at, town)); }
                    final_sync.extend(batch.into_iter().map(|bytes| (at, bytes)));
                }
                Err(why) => { log!("town: preparing the final town exchange: {why}"); }
            }
        }
        if !retiring.is_empty() {
            // Preserve parents that were ready before the cut but still in the UI's mailbox.
            let mut prior = Vec::new();
            self.link_queue.retain_mut(|(at, command)| {
                let (Some(at), ToWorker::Send(bytes)) = (at, command) else { return true };
                use makepad_town_core::share::Message;
                if let Ok(Some(Message::Heads { town, .. } | Message::HeadsDone { town, .. } | Message::Events { town, .. })) = Message::decode(bytes) {
                    if retiring.contains(&(*at, town)) {
                        prior.push((*at, std::mem::take(bytes)));
                        return false
                    }
                }
                true
            });
            prior.append(&mut final_sync);
            final_sync = prior;
        }
        for &at in self.carriers.up.keys() {
            let Carrier::Peer(peer) = at.carrier else { continue };
            if !wanted_devices.contains(&peer) && !self.carriers.towns.iter().any(|&(p, _)| p == peer) {
                final_sync.extend(model.finish_offer(peer).into_iter().map(|bytes| (at, bytes)));
            }
        }
        if !wanted.is_empty() {
            if let Err(why) = self.ensure_worker() { self.carriers.revision = None; log!("town: starting the link worker: {why}"); return }
        }
        if self.link.is_some() {
            self.link_queue.push_back((None, ToWorker::SetWanted { devices: wanted.clone(), final_sync }));
        }
        self.carriers.towns = towns;
        self.carriers.wanted = wanted;
        self.carriers.wanted_devices = wanted_devices;
    }

    fn sync_call_carriers(&mut self, cx: &mut Cx) {
        let Some(model) = &self.model else { return };
        let mut desired = BTreeMap::new();
        for &at in self.carriers.up.keys() {
            if let Carrier::Peer(peer) = at.carrier {
                if model.sync_compatible(peer) {
                    desired.insert(peer, at);
                }
            }
        }
        let detach: Vec<_> = self.carriers.calls.iter()
            .filter(|&(peer, old_at)| desired.get(peer) != Some(old_at))
            .map(|(&peer, &old_at)| (peer, old_at))
            .collect();
        for (peer, old_at) in detach {
            self.carriers.calls.remove(&peer);
            self.call.link_down(cx, peer);
            self.link_queue.push_back((Some(old_at), ToWorker::EndCall));
        }
        for (peer, at) in desired {
            if self.carriers.calls.contains_key(&peer) { continue }
            let name = &self.carriers.up[&at];
            if let Some(port) = self.call.link_up(cx, model.me(), peer, name) {
                self.carriers.calls.insert(peer, at);
                self.link_queue.push_back((Some(at), ToWorker::Call(port)));
            }
        }
        let preferred = self.call.active_peer()
            .filter(|p| self.carriers.calls.contains_key(p) && model.synced_with(p))
            .or_else(|| self.carriers.calls.keys().copied().find(|p| model.synced_with(p)))
            .or_else(|| self.call.active_peer().filter(|p| self.carriers.calls.contains_key(p)))
            .or_else(|| self.carriers.calls.keys().next().copied());
        self.call.set_preferred_peer(cx, preferred);
    }

    pub(crate) fn reconnect_start(&mut self, cx: &mut Cx) {
        self.reconcile_carriers();
        if !self.view.connect.first_contact && self.view.connect.peer.is_none() {
            if let Some(known) = self.carriers.wanted.first().cloned() { self.reach_device(cx, known); }
        }
    }

    fn reach_device(&mut self, cx: &mut Cx, known: Known) {
        self.reconcile_carriers();
        let peer = known.pin.device;
        let linked = self.carriers.up.iter().find(|(at, _)| at.carrier == Carrier::Peer(peer))
            .map(|(_, name)| name.clone());
        self.view.connect = ConnectView {
            devices: std::mem::take(&mut self.view.connect.devices),
            open: true, peer: Some(peer),
            reaching_since: linked.is_none().then(|| wall_ms() / 1000), linked, ..ConnectView::default()
        };
        self.update_presence(cx);
        self.update_connect(cx);
    }

    /// Start reaching the selected device if its carrier has not started.
    fn connect_resume(&mut self, cx: &mut Cx, device: PublicKey) {
        let Some(model) = &mut self.model else { return };
        let Some(known) = model.known_device(&device) else { return };
        let was_wanted = self.carriers.wants_device(&device);
        if let Err(e) = model.set_paused(&device, false) {
            log!("town: couldn't resume reaching the device: {e}");
            return;
        }
        if !was_wanted { self.view.retry_quiet.clear(&device); }
        self.reach_device(cx, known);
    }

    /// Ends the first contact or the link.
    pub(crate) fn connect_stop(&mut self, cx: &mut Cx) {
        if self.view.connect.first_contact {
            self.link_queue.retain(|(_, cmd)| !matches!(cmd, ToWorker::OpenWords { .. } | ToWorker::PeerWords { .. }));
            if self.link.is_some() { self.link_queue.push_back((None, ToWorker::CancelWords)); }
            if let Some(model) = &mut self.model {
                model.cancel_pair_intent();
                if let Some(peer) = self.view.connect.peer {
                    model.cancel_proved_pair(peer);
                }
            }
        }
        self.carriers.words = None;
        cx.stop_timer(std::mem::take(&mut self.pair_timer));
        self.view.connect = ConnectView {
            devices: std::mem::take(&mut self.view.connect.devices),
            ..ConnectView::default()
        };
        self.reconcile_carriers();
        self.update_presence(cx);
        self.update_town_notice(cx);
    }

    fn link_wake(&mut self, cx: &mut Cx, due: Option<std::time::Instant>) {
        if self.carriers.catch_up_timer.map(|(at, _)| at) != due {
            if let Some((_, timer)) = self.carriers.catch_up_timer.take() { cx.stop_timer(timer); }
            self.carriers.catch_up_timer = due.map(|at| {
                let delay = at.saturating_duration_since(std::time::Instant::now());
                (at, cx.start_timeout(delay.as_secs_f64()))
            });
        }
    }

    fn apply_link_status(&mut self, at: Option<Attempt>, peer: Option<PublicKey>, event: &FromWorker,
        tracked: bool, pairing: bool) -> Option<bool> {
        if !self.view.connect.apply_attempt(at, event, tracked) { return None }
        if at.is_none() && matches!(event, FromWorker::Failed(_) | FromWorker::Idle(_)) {
            self.view.retry_quiet = Default::default();
        }
        let was_quiet = peer.is_some_and(|p| self.view.retry_quiet.is_quiet(&p));
        if let Some(p) = peer { self.view.retry_quiet.on_worker_event(p, event); }
        Some(was_quiet && !pairing && matches!(event, FromWorker::Reconnecting { .. }
            | FromWorker::Phase(Status::Reconnecting | Status::Reaching { .. } | Status::Backoff { .. })))
    }

    fn drain_link_notices(&mut self, cx: &mut Cx, mut next: impl FnMut(&mut Self) -> Option<(Option<Attempt>, FromWorker)>)
        -> Option<(bool, bool, bool)> {
        let (mut changed, mut refresh, mut presence_changed) = (false, false, false);
        loop {
            let Some((at, mut event)) = next(self) else { break };
            if let FromWorker::WordsOpened(request) = event {
                if request == self.carriers.words_request && self.view.connect.first_contact { self.carriers.words = at; }
                continue;
            }
            if let FromWorker::Idle(epoch) = event {
                if self.carriers.wanted.is_empty() && self.link_queue.is_empty()
                    && self.link.as_ref().is_some_and(|w| w.configuration() == epoch) {
                    self.carriers.words = None;
                    self.view.connect.apply_attempt(None, &event, false);
                    self.view.retry_quiet = Default::default();
                    self.link = None;
                    break;
                }
                continue;
            }
            let words = at.is_some_and(|at| at.carrier == Carrier::Words);
            if words && (!self.view.connect.first_contact || at != self.carriers.words) { continue }
            if let (Some(at), FromWorker::Linked { contact, peer_device, .. }) = (at, &mut event) {
                if contact.is_some() && !self.carriers.words.is_some_and(|words| words.generation == at.generation) {
                    // A cancelled or renewed request may already have completed its proof on the worker.
                    self.carriers.revision = None;
                    if !self.carriers.wants_device(peer_device) { continue }
                    *contact = None;
                }
            }
            let promoted = matches!(&event, FromWorker::Linked { contact: Some(_), .. });
            let tracked = at.is_none() || (self.view.connect.first_contact && (words || promoted))
                || at.is_some_and(|at| matches!(at.carrier, Carrier::Peer(k) if Some(k) == self.view.connect.peer));
            let peer = at.and_then(|a| match a.carrier { Carrier::Peer(peer) => Some(peer), Carrier::Words => None });
            if let (Some(at), FromWorker::Writable) = (at, &event) {
                if self.carriers.up.contains_key(&at) { self.carriers.writable.insert(at); }
                continue
            }
            if let FromWorker::Path { peer_device, path } = &event {
                if let Some(model) = &mut self.model {
                    if let Err(e) = model.remember_path(peer_device, *path) { log!("town: remembering a carrier path: {e}"); }
                }
                continue;
            }
            if matches!(event, FromWorker::Message(_)) &&
                !at.is_some_and(|a| self.carriers.up.contains_key(&a)) {
                if let (Some(at), FromWorker::Message(_)) = (at, &event) { self.consumed_sync(at); }
                continue
            }
            if let FromWorker::Message(bytes) = &event {
                if self.document.as_ref().is_some_and(|d| d.dirty()) && self.model.as_ref().is_some_and(|m| peer.is_some_and(|p| m.peer_town_change(p, bytes)))
                    && self.flush_doc(cx).is_err() {
                    if let FromWorker::Message(bytes) = event { self.unread_link = Some((at, FromWorker::Message(bytes))); }
                    break
                }
                if let Some(at) = at { self.consumed_sync(at); }
            }
            if matches!(event, FromWorker::WordsChanged) {
                if let Some(side) = self.view.connect.side {
                    self.start_words(cx, side, true);
                    self.update_connect(cx);
                    return None
                }
            }
            let invite = matches!(event, FromWorker::Invite { .. });
            let pairing = tracked && self.view.connect.first_contact;
            let was_finding = self.view.connect.own_words.is_none();
            // A version refusal publishes Refused before Backoff. Going straight
            // from reaching to backoff means this probe heard nothing from the peer.
            let unanswered = matches!(event, FromWorker::Phase(Status::Backoff { .. }))
                && peer.is_some_and(|p| matches!(self.view.connect.device_link(&p, 0), DeviceLink::Reaching { .. }));
            let Some(quiet_retry) = self.apply_link_status(at, peer, &event, tracked, pairing) else { continue };
            if let FromWorker::PeerVersion(version) = event {
                if let Some(model) = &mut self.model {
                    let before = peer.and_then(|p| model.peer_update(&p));
                    model.peer_version(peer, version);
                    presence_changed |= peer.is_some_and(|p| model.peer_update(&p) != before && model.state.active_member(&p));
                }
                continue
            }
            if promoted { self.carriers.words = None; }
            changed |= !quiet_retry && !matches!(event, FromWorker::Message(_) | FromWorker::BlobStatus | FromWorker::BlobWorker(_));
            if invite {
                let draft = self.view.connect.side.map(|side| self.pair_form(cx, side).text_input(cx, ids!(connect_peer_words)).text()).unwrap_or_default();
                self.try_pair_draft(cx, &draft);
            }
            let Some(model) = &mut self.model else { continue };
            let previous_town = model.open_id();
            let previous = peer.map_or(ShareState::Idle, |p| model.state_for(p));
            let previous_prompt = model.offer_prompt();
            let was_joining_here = model.joining_open_town();
            let sync_town = peer.and_then(|p| model.town_for_peer(p));
            if matches!(event, FromWorker::Linked { .. } | FromWorker::Lost | FromWorker::Failed(_)) { self.view.share_error = None; }
            if unanswered || peer.is_some() && matches!(event, FromWorker::Phase(Status::Stopped) | FromWorker::Lost | FromWorker::KeyChanged | FromWorker::Failed(_)) {
                model.clear_newer_peer(peer);
            }
            match event {
                FromWorker::Linked { peer_device, version, peer_name, contact, .. } => {
                    let Some(at) = at.filter(|a| a.carrier == Carrier::Peer(peer_device)) else { continue };
                    if promoted { self.carriers.revision = None; }
                    let proved = model.link_proved(&peer_device, version, &peer_name, contact.as_ref());
                    if let Err(e) = proved {
                        if pairing {
                            log!("town: pairing the proved computer: {e}");
                            self.view.connect.failed = Some(pair_failure(&e).into());
                        } else { self.view.share_error = Some(e); }
                    } else {
                        self.carriers.up.retain(|old, _| old.carrier != at.carrier);
                        self.carriers.writable.retain(|old| old.carrier != at.carrier);
                        self.carriers.up.insert(at, peer_name);
                    }
                    refresh = true;
                }
                FromWorker::BlobWorker(endpoint) => model.attach_blobs(endpoint),
                FromWorker::Lost | FromWorker::KeyChanged => {
                    self.link_queue.retain(|(target, _)| target.is_none() || *target != at);
                    if let (Some(at), Some(peer)) = (at, peer) {
                        if self.carriers.calls.get(&peer) == Some(&at) {
                            self.carriers.calls.remove(&peer);
                            self.call.link_down(cx, peer);
                        }
                    }
                    if let Some(at) = at {
                        if self.carriers.up.remove(&at).is_some() {
                            if let Some(peer) = peer { model.end_carrier(peer, true); }
                        }
                        self.carriers.writable.remove(&at);
                    }
                    if pairing { self.view.connect.failed = Some(PAIR_FAILED.into()); }
                }
                FromWorker::Message(message) => {
                    let device = peer.expect("message from a proved carrier");
                    let allowed = model.sync_compatible(device);
                    let before_update = model.peer_update(&device);
                    match model.receive_from(device, &message) {
                    Ok(brought) => refresh |= brought,
                    Err(e) => {
                        log!("town: a sync message from the peer was refused: {e}");
                        if pairing && previous != ShareState::Idle && peer.is_some_and(|p| model.state_for(p) == ShareState::Idle) {
                            self.view.connect.failed = Some(pair_failure(&e).into());
                        }
                    }
                    }
                    presence_changed |= model.peer_update(&device) != before_update && model.state.active_member(&device);
                    if allowed != model.sync_compatible(device) {
                        let at = at.expect("message from a proved carrier");
                        if allowed {
                            model.suspend_blob_view(device);
                            self.link_queue.retain_mut(|(target, cmd)| {
                                if let ToWorker::Send(bytes) = cmd {
                                    if *target == Some(at) && !makepad_town_core::share::unprompted(bytes) {
                                        model.suspend_town_data(device, std::mem::take(bytes));
                                        self.carriers.writable.insert(at);
                                        return false
                                    }
                                }
                                if let ToWorker::SetWanted { final_sync, .. } = cmd {
                                    final_sync.retain(|(to, bytes)| *to != at || makepad_town_core::share::unprompted(bytes));
                                }
                                true
                            });
                        } else { model.attach_blob_view(at); }
                    }
                }
                FromWorker::Failed(why) => {
                    log!("town: link stopped: {why}");
                    if at.is_none() { self.link = None; self.carriers = Carriers::default(); self.link_queue.clear(); self.call.link_down_all(cx); model.sync_stop(); break; }
                    self.link_queue.retain(|(target, _)| target.is_none() || *target != at);
                    if let (Some(at), Some(peer)) = (at, peer) {
                        if self.carriers.calls.get(&peer) == Some(&at) {
                            self.carriers.calls.remove(&peer);
                            self.call.link_down(cx, peer);
                        }
                    }
                    if let Some(at) = at {
                        if self.carriers.up.remove(&at).is_some() {
                            if let Some(peer) = peer { model.end_carrier(peer, true); }
                        }
                        self.carriers.writable.remove(&at);
                    }
                    if pairing { self.view.connect.failed = Some(if was_finding { ADDRESS_FAILED } else { pair_failure(&why) }.into()); }
                }
                _ => {}
            }
            let state = peer.map_or(ShareState::Idle, |p| model.state_for(p));
            if pairing && matches!(previous, ShareState::Offered(_)) && state == ShareState::Idle && self.view.connect.failed.is_none() {
                self.view.connect.failed = Some(DECLINED.into());
            }
            let opened = !was_joining_here && model.joining_open_town();
            let agreed = matches!(state, ShareState::Shared(_)) && previous != state
                && (pairing || (previous == ShareState::Joining && !was_joining_here));
            if opened || agreed {
                if let ShareState::Shared(id) = state {
                    if id != model.open_id() {
                        if let Err(e) = model.open_town(id) { log!("town: opening the paired town: {e}"); }
                    }
                }
                self.view.connect.side = None;
                self.view.connect.first_contact = false;
                self.view.connect.own_words = None;
                self.view.page = Page::Home;
                self.home_step = HomeStep::Overview;
                refresh = true;
            }
            if model.offer_prompt().is_some() && previous_prompt != model.offer_prompt() {
                self.view.connect.side = None;
                self.view.connect.first_contact = false;
                self.view.connect.own_words = None;
                if matches!(self.home_step, HomeStep::Pair(_)) { self.home_step = HomeStep::Overview; }
            }
            if self.view.connect.side.is_none() || self.view.connect.linked.is_some() || self.view.connect.failed.is_some() {
                self.view.connect.waiting_since = None;
                if self.pair_timer.0 != 0 {
                    cx.stop_timer(std::mem::take(&mut self.pair_timer));
                }
            }
            refresh |= previous_town != model.open_id() || was_joining_here != model.joining_open_town();
            if state != previous || peer.and_then(|p| model.town_for_peer(p)) != sync_town { changed = true; }
        }
        Some((refresh, changed, presence_changed))
    }

    /// Takes what the worker said, and hands it what is due: queued commands and sync messages.
    pub(crate) fn link_event(&mut self, cx: &mut Cx) {
        self.reconcile_carriers();
        if self.link.is_none() {
            let held = crate::update_hold_of(&self.call, &self.view, &self.recorder).is_some();
            let wake = self.model.as_mut().and_then(|m| { m.set_update_hold(held); m.sources_wake(std::time::Instant::now()) });
            self.link_wake(cx, wake);
            if self.model.as_mut().is_some_and(Model::take_update_notice_changed) { self.update_town_notice(cx); }
            return
        }
        let Some((refresh, changed, presence_changed)) = self.drain_link_notices(cx, |app| {
            app.unread_link.take().or_else(|| app.link.as_ref().and_then(LinkWorker::next_event))
        }) else { return };
        self.reconcile_carriers();
        self.sync_call_carriers(cx);
        if let Some(model) = &mut self.model {
            if !model.publish_blobs() { let _ = cx.new_next_frame(); }
            let now = std::time::Instant::now();
            // a call this event started holds the check (handle_event read the hold before it)
            model.set_update_hold(crate::update_hold_of(&self.call, &self.view, &self.recorder).is_some());
            let mut next_wake = model.sources_wake(now);
            self.carriers.writable.retain(|&at| {
                let Carrier::Peer(peer) = at.carrier else { return false };
                if model.blob_policy_ready(peer) {
                    if let Some(bytes) = model.pull_sync_at(peer, now, &mut next_wake) {
                        self.link_queue.push_back((Some(at), ToWorker::Send(bytes)));
                        return false
                    }
                }
                true
            });
            self.link_wake(cx, next_wake);
        }
        // A first-contact refusal may be followed by Idle in this pump. Its form
        // still needs the refresh below after the worker has gone.
        while let Some(worker) = &self.link {
            let Some(index) = self.link_queue.iter().position(|(at, cmd)| match (at, cmd) {
                (Some(Attempt { carrier: Carrier::Peer(p), .. }), ToWorker::Send(_)) =>
                    self.model.as_ref().is_none_or(|m| m.blob_policy_ready(*p)),
                _ => true,
            }) else { break };
            let Some(command) = self.link_queue.remove(index) else { break };
            if let Err(command) = worker.send(command) {
                self.link_queue.insert(index, command);
                log!("town: the link's queue is full; the rest goes on the next frame");
                let _ = cx.new_next_frame();
                break;
            }
        }
        self.apply_link_updates(cx, refresh, changed, presence_changed);
    }

    fn apply_link_updates(&mut self, cx: &mut Cx, refresh: bool, changed: bool, presence_changed: bool) -> bool {
        let notice_changed = self.model.as_mut().is_some_and(Model::take_update_notice_changed);
        if refresh {
            self.refresh(cx);
        } else if changed {
            self.update_town_notice(cx);
            self.update_presence(cx);
            self.update_connect(cx);
        } else {
            if notice_changed { self.update_town_notice(cx); }
            if presence_changed { self.update_presence(cx); }
        }
        notice_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_town_core::key::DeviceKey;
    use makepad_network::p2p::{discover::Hint, words::Invite};

    fn press(cx: &mut Cx, widget: WidgetRef) -> ActionsBuf {
        let request = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId(widget.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        cx.capture_actions(|cx| {
            widget.borrow_mut::<Button>().unwrap().handle_action(cx, &request, &mut Scope::empty());
        })
    }

    #[test]
    fn home_flows_follow_the_town_and_leave_pairing_running() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, {
                use mod.widgets.*
                View{
                    name_prompt := PersonNamePrompt{}
                    home_problem := HomeProblem{}
                    workspace := View{home_page := HomePage{visible: true} join_page := JoinPage{} chat_panel := ChatRail{}
                        // The rail's footer button, with the action data the rail gives it.
                        connect_join := Button{text: "Join a town"}}
                }
            });
            WidgetRef::script_from_value(vm, root)
        });
        let key = DeviceKey::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("create-pair-panel-{}", makepad_town_core::hex(&key.public())));
        let label_path = dir.join("local/computer-label");
        let marker = dir.join("local/name-chosen");
        app.model = Some(Model::open(&dir).unwrap().0);
        app.show_name_prompt(cx);
        let prompt = app.ui.widget(cx, ids!(name_prompt));
        let workspace = app.ui.widget(cx, ids!(workspace));
        let field = prompt.text_input(cx, ids!(person_name));
        let suggestion = makepad_town_core::home::Home::suggested_person_name();
        assert!(prompt.visible() && !workspace.visible(), "a fresh home asks for a name");
        assert_eq!(field.text(), suggestion.as_deref().unwrap_or(""), "a missing label uses the OS suggestion or an empty field");
        assert!(!label_path.try_exists().unwrap(), "prefilling the prompt leaves a missing label file absent");
        let model = app.model.as_ref().unwrap();
        let fallback = model.home.computer_label().expect("a startup caller reads the fallback label");
        assert_eq!(model.home.chosen_computer_label().unwrap(), None, "reading the fallback does not choose a name");
        assert_eq!(model.home.name_prompt_prefill(), suggestion, "a stored hostname still uses the OS suggestion");
        model.home.set_open_town(&model.open_id()).unwrap();
        let (other, genesis) = makepad_town_core::event::Event::found(&model.device_key(), 2, 0, 0, [7; 16]).unwrap();
        makepad_town_core::log::Log::found(&model.home.town_log_path(&other), genesis).unwrap();
        drop(app.model.take());
        let stored = if fallback == "Ben" { "Ana" } else { "Ben" };
        std::fs::write(&label_path, stored).unwrap();
        app.model = Some(Model::open(&dir).unwrap().0);
        app.show_name_prompt(cx);
        assert!(prompt.visible() && !workspace.visible(), "a custom label without the marker still opens the prompt");
        assert_eq!(field.text(), stored, "the prompt offers the home's stored name");
        assert!(!marker.try_exists().unwrap(), "showing a stored name leaves the choice pending");
        field.set_text(cx, "Ana");
        let submit = prompt.widget(cx, ids!(name_continue));
        let clicks = press(cx, submit);
        app.handle_actions(cx, &clicks);
        assert!(workspace.visible() && !prompt.visible(), "Continue reveals the workspace");
        app.model.as_ref().unwrap().home.settle_writer();
        assert!(marker.try_exists().unwrap(), "Continue records the choice in the marker");
        assert_eq!(app.model.as_ref().unwrap().home.computer_label().unwrap(), "Ana", "Continue saves the pairing label");
        assert_eq!(app.view.members.seats[0].name, "Ana", "the person's chip shows the chosen name");
        drop(app.model.take());
        // Another Town still holds the home, as when one is closing while this one starts.
        let other_town = makepad_town_core::home::Home::open(&dir).unwrap();
        app.home_dir = Some(dir.clone());
        assert!(!app.open_model(cx) && app.model.is_none() && !app.home_wait.is_empty(), "a busy home is tried again later");
        let problem = app.ui.widget(cx, ids!(home_problem));
        assert!(problem.visible() && !workspace.visible() && !prompt.visible(), "no town shows while the home is busy, only why");
        assert!(problem.label(cx, ids!(home_problem_text)).text().starts_with("Another Town is using"), "the line says what holds it");
        drop(other_town);
        assert!(app.open_model(cx) && app.model.is_some() && app.problem.is_none(), "the next try opens the home it left");
        app.show_name_prompt(cx);
        assert!(!prompt.visible() && !problem.visible() && workspace.visible(), "a reopened home skips the name prompt");
        app.read_workspace();
        let open = app.model.as_ref().unwrap().open_id();
        assert!(app.view.tree.lines.iter().any(|line| line.node == makepad_create::lists::Node::Town(open)), "the sidebar lists the open town");
        // Words already being found, so the press opens the page without reaching the address reflectors.
        app.view.connect = ConnectView { open: true, first_contact: true, side: Some(PairSide::Join), ..ConnectView::default() };
        let join = app.ui.button(cx, ids!(connect_join));
        join.set_action_data(HomeAction::Join);
        let clicks = press(cx, app.ui.widget(cx, ids!(connect_join)));
        app.connect_actions(cx, &clicks);
        assert!(app.view.page == Page::Join && app.pair_step_visible() && app.ui.widget(cx, ids!(join_page)).visible(), "Join opens its page");
        let clicks = press(cx, app.ui.widget(cx, ids!(join_page.pair_close)));
        app.connect_actions(cx, &clicks);
        assert_eq!(app.view.page, Page::Home, "closing Join returns to the town");
        let model = app.model.as_mut().unwrap();
        model.set_person_name("").unwrap();
        assert_eq!(model.home.chosen_computer_label().unwrap(), Some(makepad_town_core::home::Home::default_computer_label()),
            "accepting the fallback counts as choosing a name even when it matches the hostname");
        let content = app.ui.widget(cx, ids!(home_page));
        let home_action = |app: &mut App, cx: &mut Cx, button| {
            let clicks = press(cx, button);
            app.connect_actions(cx, &clicks);
        };

        // The worker's address result. Exercise the form without an external reflector.
        let addr = "192.0.2.1:4242".parse().unwrap();
        let own = Invite { nat: Hint::Easy, addr, lan: None, commit: 7 };
        let peer_words = Invite { commit: 8, ..own.clone() }.to_words().join(" ");
        app.read_workspace();
        let town = app.model.as_ref().unwrap().open_id();
        assert_ne!(town, other);
        let members = app.ui.widget(cx, ids!(home_members));
        let draw_home = |app: &mut App, cx: &mut Cx| {
            use makepad_widgets::cx_draw::CxDraw;
            app.update_presence(cx);
            let pass = DrawPass::new(cx);
            let size = dvec2(650., 160.);
            pass.set_size(cx, size);
            let mut list = DrawList2d::new(cx);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            content.draw_all(&mut frame, &mut Scope::with_data(&mut app.view));
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
        };
        app.remote_binary = Some(makepad_create::view::RemoteBinary::Town(dir.join("Town")));
        app.endpoint = Some(dir.join("endpoint"));
        let click = |app: &mut App, cx: &mut Cx, path| {
            let button = content.widget(cx, path);
            home_action(app, cx, button);
        };
        let mut previous: Option<(ActionsBuf, ActionsBuf)> = None;
        for target in [town, other, town] {
            let title = content.text_input(cx, ids!(home_title));
            if previous.is_some() {
                title.take_key_focus(cx);
                cx.action(());
                cx.handle_actions();
                assert!(cx.has_key_focus(title.area()), "a collaborator can deliberately focus the current town's title");
            }
            app.switch_town(cx, target);
            cx.action(());
            cx.handle_actions();
            draw_home(&mut app, cx);
            assert!(title.area().is_valid(cx), "the opened town has a drawn title field");
            assert!(!cx.has_key_focus(title.area()), "opening or switching towns must not start a rename");
            assert!(!title.borrow().unwrap().draw_cursor.area().is_valid(cx), "the unfocused town title must not draw a caret");
            click(&mut app, cx, ids!(add_member));
            let ai = content.widget(cx, ids!(home_ai));
            let pair = content.widget(cx, ids!(home_pair));
            let mine = content.widget(cx, ids!(home_pair_mine));
            assert!(content.widget(cx, ids!(add_menu)).visible() && pair.visible() && mine.visible());
            assert_eq!((pair.text(), mine.text(), ai.text()),
                ("Someone else's computer".into(), "Another computer of mine".into(), "Add my AI".into()));
            if let Some((ai_click, pair_click)) = previous.take() {
                app.connect_actions(cx, &ai_click);
                app.connect_actions(cx, &pair_click);
                assert_eq!(app.home_step, HomeStep::AddMenu(target), "a click from another town cannot start either invitation");
            }
            previous = Some((press(cx, ai.clone()), press(cx, pair)));
            home_action(&mut app, cx, ai);
            assert!(content.widget(cx, ids!(ai_section)).visible(), "the invitation appears inline");
            assert!(content.widget(cx, ids!(home_grid)).visible(), "projects stay below the AI invitation");
            let phase = &app.hooks[&target].phase;
            let HookPhase::Waiting { code, .. } = phase else { panic!("expected a waiting invitation") };
            let full = phase.clipboard_text().expect("Copy has a sentence for the waiting invitation");
            let (command, argument) = full.rsplit_once(r#" call town.join '{"code":""#).expect("Copy includes the join command");
            let bearer = argument.strip_suffix(r#""}' and follow what it prints."#).expect("Copy includes the full instruction");
            assert!(code.town == target && command.contains("' remote app ") && Credential::parse(bearer) == Some(*code),
                "Copy carries the unmasked code issued for this town");
            assert!(content.button(cx, ids!(ai_copy)).visible(), "Copy is available beside the invitation");
            let masked = format!("“{}”", full.replace(bearer, "••••••"));
            let shown = format!("“{full}”");
            let preview = content.label(cx, ids!(ai_preview));
            let show = content.button(cx, ids!(ai_show));
            let show_name = |cx: &Cx| {
                let mut node = a11y_tree::Node::default();
                show.borrow().unwrap().semantics(cx, &mut node);
                (show.text(), node.name)
            };
            assert!(preview.text() == masked && show.visible(), "opening or returning to a town starts with its code masked");
            assert_eq!(show_name(cx), ("Show".into(), "Show the join code".into()), "the button names the reveal action");
            click(&mut app, cx, ids!(ai_show));
            assert!(preview.text() == shown, "Show reveals exactly the sentence Copy gives the AI");
            assert_eq!(show_name(cx), ("Hide".into(), "Hide the join code".into()), "the revealed code offers Hide");
            click(&mut app, cx, ids!(ai_show));
            assert!(preview.text() == masked, "Hide restores the six bullets");
            click(&mut app, cx, ids!(ai_show)); // Leave it shown before switching towns or closing.
        }
        let code = |app: &App| match app.hooks[&town].phase {
            HookPhase::Waiting { code, .. } => code, _ => panic!("expected a waiting sentence"),
        };
        let status = content.label(cx, ids!(ai_status));
        let old_show = press(cx, content.widget(cx, ids!(ai_show)));
        let stop_button = content.widget(cx, ids!(ai_close));
        let old_stop = press(cx, stop_button);
        let withdrawn = code(&app);
        click(&mut app, cx, ids!(ai_close));
        assert!(content.label(cx, ids!(ai_preview)).text().is_empty(), "closing clears the previously revealed sentence");
        assert!(app.model.as_mut().unwrap().agent_join(withdrawn).is_err());
        assert!(!content.widget(cx, ids!(ai_section)).visible(), "closing withdraws the unused invitation and hides its panel");
        app.remote_binary = None;
        click(&mut app, cx, ids!(add_member));
        click(&mut app, cx, ids!(home_ai));
        assert_eq!(status.text(), "Town couldn't find makepad-remote.");
        app.remote_binary = Some(makepad_create::view::RemoteBinary::Town(dir.join("Town")));
        click(&mut app, cx, ids!(ai_retry));
        let expired = code(&app);
        app.connect_actions(cx, &old_show);
        assert!(content.button(cx, ids!(ai_show)).text() == "Show" && content.label(cx, ids!(ai_preview)).text().contains("••••••"),
            "Try again starts masked and a Show press from the withdrawn invitation cannot reveal it");
        app.connect_actions(cx, &old_stop);
        assert_eq!(code(&app), expired, "an old Stop cannot withdraw a fresh sentence");
        assert_ne!(expired, withdrawn);
        let end = match app.hooks[&town].phase { HookPhase::Waiting { expires, .. } => expires, _ => unreachable!() };
        app.expire_hooks(cx, end);
        assert_eq!(status.text(), "That sentence has expired.");
        assert!(app.model.as_mut().unwrap().agent_join(expired).is_err(), "the timer withdraws the code before a retry");
        click(&mut app, cx, ids!(ai_retry));
        let refused = code(&app);
        assert!(app.join_hook(cx, expired).is_err());
        let file = app.model.as_ref().unwrap().home.agents_path(&town);
        let saved = file.with_extension("saved");
        std::fs::rename(&file, &saved).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(app.join_hook(cx, refused).is_err());
        app.update_connect(cx);
        assert_eq!(status.text(), "Town couldn't let your AI join.");
        std::fs::remove_dir(&file).unwrap();
        std::fs::rename(saved, file).unwrap();
        click(&mut app, cx, ids!(ai_retry));
        let fresh = code(&app);
        assert_ne!(fresh, refused);
        let token = app.join_hook(cx, fresh).unwrap();
        assert!(app.model.as_mut().unwrap().agent_join(refused).is_err());
        app.call(cx, makepad_ai_services::wire::ServiceCall {
            call_id: "session".into(), tool: "wait".into(),
            args: makepad_strict_json::obj(vec![("binding", makepad_strict_json::s(token.clone()))]).to_json(),
            caller: None, origin: makepad_ai_services::wire::CallOrigin::Remote,
        });
        assert!(!content.widget(cx, ids!(ai_section)).visible(), "the held wait closes the invitation without another click");
        let agent = app.model.as_ref().unwrap().my_agent(None).unwrap();
        assert!(app.view.members.seats.iter().any(|s| s.id == MemberId::Agent(agent) && s.presence == makepad_create::view::Presence::Here), "the joined AI has its own online chip");
        for text in ["ordinary chat", "@unknown hello"] {
            assert!(app.send_chat(cx, text.into()), "the composer accepts the town message {text:?}");
            let line = app.model.as_ref().unwrap().state.chat.last().unwrap();
            assert_eq!((line.chat.text.as_str(), line.to_agent, line.chat.to_ai), (text, None, false),
                "a plain line keeps its text and addresses no AI");
            assert!(app.view.workspace.connect.rows.iter().any(|a| a.live), "the AI keeps waiting after a plain line");
        }
        assert!(app.send_chat(cx, "@ai hello".into()));
        assert!(app.model.as_ref().unwrap().pending(None).unwrap().is_empty());
        assert!(app.view.workspace.connect.rows.iter().any(|a| a.status == "answering…"));

        draw_home(&mut app, cx);
        let member_chip = |cx: &Cx, member| {
            let list = members.flat_list(cx, ids!(list));
            let list = list.borrow().unwrap();
            list.items.iter().filter(|(_, item)| item.template == id!(Seat)).map(|(_, item)| item.widget.widget(cx, ids!(select)))
                .find(|chip| matches!(chip.action_data().as_ref().and_then(|d| d.downcast_ref::<HomeAction>()),
                    Some(HomeAction::Card { member: id, .. }) if *id == member)).expect("visible member chip")
        };
        let chip = member_chip(cx, MemberId::You);
        home_action(&mut app, cx, chip);
        let offer = content.button(cx, ids!(hub_offer));
        assert!(offer.visible() && offer.text() == "Offer my AI Hub to this town", "a fresh owner card offers the switch");
        click(&mut app, cx, ids!(hub_offer));
        assert_eq!(offer.text(), "Stop offering my AI Hub", "the owner's card reads back the signed offer");
        assert!(content.widget(cx, ids!(save_label)).is_empty(), "the name field needs no Rename button");
        draw_home(&mut app, cx);
        let name = content.text_input(cx, ids!(computer_label));
        assert!(name.visible() && !content.widget(cx, ids!(member_name)).visible(), "your name is the field itself");
        name.take_key_focus(cx);
        cx.action(());
        cx.handle_actions();
        name.borrow_mut().unwrap().select_all(cx);
        let enter = cx.capture_actions(|cx| {
            content.handle_event(cx, &Event::TextInput(TextInputEvent { input: "Kit".into(), ..Default::default() }), &mut Scope::empty());
            content.handle_event(cx, &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }), &mut Scope::empty());
        });
        app.connect_actions(cx, &enter);
        assert_eq!(app.model.as_ref().unwrap().home.computer_label().unwrap(), "Kit", "Enter saves the typed name");
        let card = content.widget(cx, ids!(member_card));
        let escape = Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() });
        let chip = member_chip(cx, MemberId::You);
        home_action(&mut app, cx, chip);
        draw_home(&mut app, cx);
        name.take_key_focus(cx);
        cx.action(());
        cx.handle_actions();
        assert!(card.visible() && name.visible() && !name.area().is_empty() && cx.has_key_focus(name.area()),
            "the name field has focus when Escape is pressed");
        let escaped = cx.capture_actions(|cx| content.handle_event(cx, &escape, &mut Scope::empty()));
        app.connect_actions(cx, &escaped);
        cx.action(());
        cx.handle_actions();
        assert!(!card.visible() && !name.visible() && !cx.has_key_focus(name.area()),
            "Escape closes the card, hides the name field and releases its focus");
        let chip = member_chip(cx, MemberId::Agent(agent));
        home_action(&mut app, cx, chip);
        assert_eq!(content.label(cx, ids!(member_name)).text(), "Your AI · Claude Code", "the card names the AI and its kind");
        let ai_status = &app.view.members.seats.iter().find(|s| s.id == MemberId::Agent(agent)).unwrap().status;
        assert_eq!(&content.label(cx, ids!(member_status)).text(), ai_status, "the AI detail uses its chip's status");
        assert!(content.widget(cx, ids!(member_more)).is_empty() && content.button(cx, ids!(remove_ai)).visible(),
            "the AI card offers Remove directly");

        // Once the reply finishes, the same member can be brought back with a new binding.
        let binding = Credential::parse(&token).unwrap();
        app.model.as_mut().unwrap().agent_say(binding, "done".into(), None).unwrap();
        app.refresh(cx);
        click(&mut app, cx, ids!(add_member));
        assert_eq!(content.button(cx, ids!(home_ai)).text(), "Bring my AI back", "the menu offers to rehook an away member");
        click(&mut app, cx, ids!(home_ai));
        assert_eq!(app.hooks[&town].target, Some(agent), "the invitation reuses the away member's identity");
        click(&mut app, cx, ids!(ai_close));
        let chip = member_chip(cx, MemberId::Agent(agent));
        home_action(&mut app, cx, chip);
        click(&mut app, cx, ids!(remove_ai));
        assert!(!app.model.as_ref().unwrap().state.agents.contains_key(&agent) && app.removing.is_none(),
            "one press removes the AI immediately");


        app.view.connect = ConnectView { open: true, first_contact: true, side: Some(PairSide::Add(town, AddComputer::SomeoneElse)), ..ConnectView::default() };
        app.view.connect.apply(&FromWorker::Invite { words: own.to_words() });
        click(&mut app, cx, ids!(add_member));
        let pair = content.widget(cx, ids!(home_pair));
        home_action(&mut app, cx, pair);
        assert_eq!(content.label(cx, ids!(pair_title)).text(), "Add a computer to Center", "a new home's town is Center");
        app.view.connect.side = Some(PairSide::Join);
        app.view.page = Page::Join;
        app.update_page(cx);
        let join = app.ui.widget(cx, ids!(join_page));
        assert!(join.visible() && !content.visible(), "joining is a page of its own that shows nothing of the open town");
        assert_eq!(join.label(cx, ids!(pair_title)).text(), "Join a town on another computer");
        let input = join.text_input(cx, ids!(connect_peer_words));
        let own_words = join.label(cx, ids!(pair_words)).text();
        assert!(!own_words.is_empty());
        for waiting in [false, true] {
            if waiting {
                let change = cx.capture_actions(|cx| input.replace_range(cx, 0..0, &peer_words, UndoGroup::New).unwrap());
                app.connect_actions(cx, &change);
            }
            let started = app.model.as_ref().unwrap().pairing_since();
            assert_eq!(started.is_some(), waiting, "a valid checksum starts pairing on the edit");
            app.view.page = Page::Project;
            app.update_page(cx);
            assert!(!app.pair_step_visible());
            app.open_pairing(cx, PairSide::Join);
            assert!(app.pair_step_visible());
            assert_eq!(app.model.as_ref().unwrap().pairing_since(), started);
            assert_eq!(join.label(cx, ids!(pair_words)).text(), own_words);
            assert_eq!(input.text(), if waiting { &peer_words } else { "" }, "returning to Join preserves the pairing draft");
            assert_eq!(join.label(cx, ids!(pair_status)).text().starts_with("Waiting for the other computer…"), waiting);
        }
        // A computer this one linked with before, at an address the link remembered.
        let pool = DeviceKey::generate().unwrap().public();
        let model = app.model.as_mut().unwrap();
        model.pin_linked(&pool, 3, "Pool box").unwrap();
        let path = makepad_town_core::peers::PathMemory { addr: Some("192.0.2.2:4243".parse().unwrap()), lan: None };
        model.remember_path(&pool, path).unwrap();
        let stop = join.widget(cx, ids!(pair_close));
        home_action(&mut app, cx, stop);
        assert!(!app.pair_step_visible() && app.view.page == Page::Home, "closing Join returns to the open town");
        assert!(app.view.connect.side.is_none() && app.view.connect.own_words.is_none() && app.link_queue.is_empty());
        assert!(app.model.as_ref().unwrap().pairing_since().is_none());
        assert!(app.view.connect.peer.is_none() && app.carriers.wanted.is_empty(),
            "stopping words leaves a pinned computer outside every shared town silent");
        app.connect_stop(cx);

        let mut peer = Model::open(&dir.join("peer")).unwrap().0;
        let private_town = peer.open_id();
        let model = app.model.as_mut().unwrap();
        let peer_key = peer.me();
        model.pin_linked(&peer_key, 3, "River").unwrap();
        model.remember_path(&peer_key, path).unwrap();
        peer.pin_linked(&model.me(), 3, "Art desk").unwrap();
        model.link_proved(&peer_key, 3, "River", None).unwrap();
        peer.link_proved(&model.me(), 3, "Art desk", None).unwrap();
        let hello = model.pull_sync(peer_key).expect("the new carrier greets the peer");
        peer.receive_from(model.me(), &hello).unwrap();
        let hello = peer.pull_sync(model.me()).expect("the peer greets the new carrier");
        model.receive_from(peer_key, &hello).unwrap();
        peer.offer_town(true).unwrap();
        let offer = peer.pull_sync(model.me()).expect("the peer offers a new town");
        model.receive_from(peer.me(), &offer).unwrap();
        app.update_presence(cx);
        app.update_town_notice(cx);
        app.update_connect(cx);
        assert!(content.widget(cx, ids!(share_incoming)).visible());
        assert_eq!(content.label(cx, ids!(offer_line)).text(), "River wants to add a town to this computer.");
        assert!(!content.widget(cx, ids!(town_notice)).visible(), "the offer is the home page's prompt, shown once");
        {
            assert!(app.ui.widget(cx, ids!(chat_panel)).widget(cx, ids!(town_notice)).is_empty(), "no notice sits above the chat");
        }
        let accept = content.widget(cx, ids!(share_join));
        let stale = press(cx, accept);
        let decline = content.widget(cx, ids!(share_decline));
        let clicks = press(cx, decline);
        app.town_notice_actions(cx, &clicks);
        assert!(!content.widget(cx, ids!(share_incoming)).visible());
        assert!(app.model.as_ref().unwrap().offer_prompt().is_none());
        let model = app.model.as_mut().unwrap();
        while model.pull_sync(peer.me()).is_some() {} // Drain the declined offer.
        model.receive_from(peer.me(), &offer).unwrap();
        let fresh = model.offer_prompt().unwrap();
        app.update_connect(cx);
        app.town_notice_actions(cx, &stale);
        assert_eq!(app.model.as_ref().unwrap().offer_prompt(), Some(fresh));
        let accept = content.widget(cx, ids!(share_join));
        let clicks = press(cx, accept);
        app.town_notice_actions(cx, &clicks);
        assert_eq!(app.model.as_ref().unwrap().share_state(), ShareState::Joining);
        let model = app.model.as_mut().unwrap();
        loop {
            let outgoing = model.pull_sync(peer_key);
            let incoming = peer.pull_sync(model.me());
            if outgoing.is_none() && incoming.is_none() { break }
            if let Some(bytes) = outgoing { peer.receive_from(model.me(), &bytes).unwrap(); }
            if let Some(bytes) = incoming { model.receive_from(peer_key, &bytes).unwrap(); }
        }
        peer.act(Action::Provide { on: true }, Origin::Ui).unwrap();
        while let Some(bytes) = peer.pull_sync(model.me()) { model.receive_from(peer_key, &bytes).unwrap(); }
        let ShareState::Shared(shared) = model.share_state() else { panic!("both peers should have joined the offered town") };
        app.switch_town(cx, shared);
        draw_home(&mut app, cx);
        let tip = members.widget(cx, ids!(member_tip));
        assert_eq!(app.model.as_ref().unwrap().pair_problem(shared), None);
        assert_eq!(tip.borrow::<tip::Tip>().unwrap().text, "Add a member");
        click(&mut app, cx, ids!(add_member));
        assert!(content.widget(cx, ids!(home_pair)).visible() && content.widget(cx, ids!(home_pair_mine)).visible()
            && content.widget(cx, ids!(home_ai)).visible(), "a shared town offers all three choices");
        assert!(tip.borrow::<tip::Tip>().unwrap().text.is_empty(), "the plus cannot raise its tooltip over the open menu");
        click(&mut app, cx, ids!(home_pair_mine));
        assert_eq!(app.view.connect.side, Some(PairSide::Add(shared, AddComputer::Mine)));
        assert!(app.pair_step_visible());
        let pair_step = content.widget(cx, ids!(pair_step));
        let chip = member_chip(cx, MemberId::Computer(peer_key));
        home_action(&mut app, cx, chip);
        assert_eq!(app.view.selected_member, Some(MemberId::Computer(peer_key)), "the peer chip owns the card");
        assert!(card.visible() && !pair_step.visible(), "the peer card temporarily hides the pairing form");
        app.handle_event(cx, &escape);
        assert!(!card.visible() && app.view.selected_member.is_none(), "Escape closes the peer card and clears its selection");
        assert_eq!(app.home_step, HomeStep::Pair(PairSide::Add(shared, AddComputer::Mine)));
        assert!(app.pair_step_visible() && pair_step.visible(), "Escape restores the pending pairing form");
        click(&mut app, cx, ids!(pair_close));
        let chip = member_chip(cx, MemberId::Computer(peer_key));
        home_action(&mut app, cx, chip);
        assert_eq!(tip.borrow::<tip::Tip>().unwrap().text, "Add a member");
        assert!(!content.button(cx, ids!(hub_offer)).visible(), "another computer has no owner switch");
        let status = content.label(cx, ids!(member_status));
        let link = content.button(cx, ids!(device_link));
        let remove = content.button(cx, ids!(remove_member));
        let line = content.label(cx, ids!(removing_line));
        let undo = content.button(cx, ids!(undo_remove));
        assert_eq!(status.text(), "here · offers summaries and task updates", "the signed services share the presence line");
        assert!(!link.visible() && !remove.visible(), "a joiner cannot remove the founder or reconnect a linked computer");
        assert!(content.widget(cx, ids!(device_disconnect)).is_empty(), "the card has no Disconnect control");

        let you = member_chip(cx, MemberId::You);
        home_action(&mut app, cx, you);
        assert!(remove.visible() && remove.text() == "Leave", "a joiner can leave from their own card");
        click(&mut app, cx, ids!(remove_member));
        assert!(line.text() == "Leaving this town — this device can't rejoin." && undo.visible(), "Leave offers Undo in place");
        click(&mut app, cx, ids!(undo_remove));
        assert!(remove.visible() && app.model.as_ref().unwrap().state.active_member(&app.model.as_ref().unwrap().me()),
            "Undo keeps the joiner in the town");

        // Inspect the founder's copy of the same town after the carrier ends.
        let guest = app.model.as_ref().unwrap().me();
        peer.open_town(shared).unwrap();
        peer.remember_path(&guest, path).unwrap();
        peer.end_carrier(guest, true);
        std::mem::swap(app.model.as_mut().unwrap(), &mut peer);
        app.refresh(cx);
        draw_home(&mut app, cx);
        let chip = member_chip(cx, MemberId::Computer(guest));
        home_action(&mut app, cx, chip.clone());
        assert!(card.visible(), "the guest chip opens its card");
        assert_eq!(app.view.selected_member, Some(MemberId::Computer(guest)));
        app.handle_event(cx, &escape);
        assert_eq!(app.home_step, HomeStep::Overview, "Escape returns to the overview when no invitation is pending");
        assert!(!card.visible() && app.view.selected_member.is_none(), "Escape clears the guest's card and selection");
        home_action(&mut app, cx, chip);
        assert!(link.visible() && remove.visible() && remove.text() == "Remove", "an unstarted guest carrier offers Connect and founder Remove");
        let guest_name = content.label(cx, ids!(member_name)).text();
        let queued = app.link_queue.len();
        click(&mut app, cx, ids!(device_link));
        assert_eq!(app.view.connect.peer, Some(guest), "Connect reaches the computer on this card");
        assert_eq!(app.link_queue.len(), queued + 1);
        assert!(matches!(app.link_queue.back(), Some((None, ToWorker::SetWanted { .. }))) && !link.visible());

        use makepad_town_core::{link::VERSION, peers::{Peers, TownState}, share::{hello::Hello, Message}};
        fn pump(app: &mut App, cx: &mut Cx, at: Option<Attempt>, events: impl IntoIterator<Item = FromWorker>)
            -> (bool, bool, bool, bool) {
            let mut events = events.into_iter();
            let (refresh, changed, presence_changed) = app.drain_link_notices(cx, |_| events.next().map(|event| (at, event)))
                .expect("carrier events run to completion");
            let notice_changed = app.apply_link_updates(cx, refresh, changed, presence_changed);
            (refresh, changed, presence_changed, notice_changed)
        }
        let attempt = |generation| Some(Attempt { carrier: Carrier::Peer(guest), generation });
        let reconnect = |since| FromWorker::Reconnecting { device: guest, since };
        let phase = FromWorker::Phase;
        let backoff = |retry_at| phase(Status::Backoff { retry_at });
        let updates = |app: &App| (app.view.member_revision, app.presence_updates);
        let guest_status = |app: &App| {
            let seat = app.view.members.seats.iter().find(|s| s.id == MemberId::Computer(guest)).unwrap();
            (seat.presence, seat.status.clone())
        };
        let quiet = (false, false, false, false);
        let changed = (false, true, false, false);
        let now = wall_ms() / 1000;
        let (at1, at2, at3) = (attempt(1), attempt(2), attempt(3));
        pump(&mut app, cx, at1, [reconnect(now - 125), phase(Status::Reconnecting)]);
        assert_eq!(guest_status(&app), (Presence::Reaching, "reaching for 2 min".into()));
        assert_eq!(status.text(), "reaching for 2 min", "a presence update refreshes the open card");
        assert!(!link.visible() && remove.visible());
        assert_eq!(pump(&mut app, cx, at1, [backoff(now + 17)]), changed, "the first unanswered probe settles the row");
        assert!(app.view.retry_quiet.is_quiet(&guest));
        assert_eq!(guest_status(&app), (Presence::Away, status.text()));
        assert!(status.text().starts_with("away · last seen ") && !link.visible() && remove.visible());

        let queued = app.link_queue.len();
        app.connect_resume(cx, guest);
        assert_eq!(app.link_queue.len(), queued, "resuming a wanted carrier queues nothing");
        assert!(app.view.retry_quiet.is_quiet(&guest) && !link.visible());
        let before = updates(&app);
        assert_eq!(pump(&mut app, cx, at1, [backoff(now + 34)]), quiet);
        // Losing the socket ends the connection. The old attempt publishes Backoff before the new slot starts.
        assert_eq!(pump(&mut app, cx, at1, [backoff(now + 35)]), quiet);
        assert_eq!(pump(&mut app, cx, at2, [reconnect(now + 35), phase(Status::Reconnecting)]), quiet);
        assert_eq!(before, updates(&app), "blind restarts leave the presence timer and people row alone");
        assert!(app.view.retry_quiet.is_quiet(&guest) && guest_status(&app).0 == Presence::Away);
        assert_eq!(pump(&mut app, cx, at2, [phase(Status::Securing)]), changed);
        assert!(!app.view.retry_quiet.is_quiet(&guest));
        assert_eq!(guest_status(&app), (Presence::Reaching, "reaching…".into()));
        assert!(!link.visible());
        assert_eq!(pump(&mut app, cx, at2, [backoff(now + 37)]), changed);
        assert!(app.view.retry_quiet.is_quiet(&guest));

        // A recorded departure excludes this pair even while the folded town still lists the guest.
        let mut pins = Peers::open(&app.model.as_ref().unwrap().home).unwrap();
        pins.add_town(shared, now).unwrap();
        pins.set_town_state(&shared, TownState::Left).unwrap();
        app.model.as_mut().unwrap().devices_retry().unwrap();
        app.reconcile_carriers();
        assert!(!app.carriers.wants_device(&guest));
        assert_eq!(pump(&mut app, cx, at3, [phase(Status::Stopped)]), changed);
        assert!(!app.view.retry_quiet.is_quiet(&guest));
        assert_eq!(app.apply_link_status(at2, Some(guest), &backoff(now + 54), true, false), None);
        assert!(!app.view.retry_quiet.is_quiet(&guest), "retired attempts cannot put a stopped peer back in quiet");
        pump(&mut app, cx, None, [FromWorker::Failed("worker stopped".into())]);
        assert!(!link.visible() && remove.visible(), "teardown cannot offer Connect on an excluded pair");
        pins.set_town_state(&shared, TownState::Active).unwrap();
        app.model.as_mut().unwrap().devices_retry().unwrap();
        app.update_connect(cx);
        assert!(link.visible() && remove.visible(), "the wanted pair has no running carrier");
        let queued = app.link_queue.len();
        click(&mut app, cx, ids!(device_link));
        assert_eq!(app.link_queue.len(), queued + 1);
        assert!(matches!(app.link_queue.back(), Some((None, ToWorker::SetWanted { .. }))) && !link.visible() && remove.visible());

        // The replacement worker starts its own generations. A proof refusal precedes its retry deadline.
        let notice = content.widget(cx, ids!(town_notice));
        let notice_text = content.label(cx, ids!(town_notice_text));
        let proof = [reconnect(now), phase(Status::Reconnecting), phase(Status::Securing), phase(Status::Proving)];
        pump(&mut app, cx, at1, proof);
        assert_eq!(pump(&mut app, cx, at1, [
            FromWorker::PeerVersion(VERSION + 1), FromWorker::Refused("refused".into()), backoff(now + 17),
        ]), (false, true, true, true));
        assert!(app.view.retry_quiet.is_quiet(&guest));
        assert_eq!(guest_status(&app), (Presence::Away, "your Town is older".into()));
        assert_eq!(status.text(), "your Town is older");
        assert!(notice.visible() && !link.visible() && remove.visible());
        assert!(notice_text.text().contains("Your copy of Makepad Town is older than "));
        let rebuild = "pull makepad and apps, then rebuild";
        assert!(notice_text.text().contains(rebuild));

        let before = updates(&app);
        assert_eq!(pump(&mut app, cx, at2, [reconnect(now + 17), phase(Status::Reconnecting)]), quiet);
        assert_eq!(updates(&app), before);
        assert_eq!(pump(&mut app, cx, at2, [backoff(now + 94)]), (false, false, false, true));
        assert_eq!(updates(&app), before, "clearing the rebuild notice must not call update_presence");
        assert!(notice.visible() && notice_text.text().contains("Your copy of Makepad Town is older than "));
        assert!(!notice_text.text().contains(rebuild));
        assert!(app.view.retry_quiet.is_quiet(&guest));
        assert_eq!(guest_status(&app), (Presence::Away, "your Town is older".into()));
        assert_eq!(pump(&mut app, cx, at2, [backoff(now + 111)]), quiet);
        assert_eq!(updates(&app), before, "later unanswered wakes do no UI work");

        // An answering peer wakes this same connection. Only the compatible Hello clears the mismatch.
        pump(&mut app, cx, at2, [
            reconnect(now + 111), phase(Status::Reconnecting), phase(Status::Securing), phase(Status::Proving),
            FromWorker::Linked { peer_device: guest, version: VERSION, peer_name: guest_name.clone(), contact: None },
            phase(Status::Linked),
        ]);
        assert!(!app.view.retry_quiet.is_quiet(&guest));
        assert_eq!(status.text(), "your Town is older");
        let (revision, calls) = updates(&app);
        let hello = Message::Hello(Hello::local(std::iter::empty())).encode();
        assert_eq!(pump(&mut app, cx, at2, [FromWorker::Message(hello)]), (false, false, true, true));
        assert_eq!(updates(&app), (revision + 1, calls + 1));
        assert_eq!(guest_status(&app), (Presence::Here, "here".into()));
        assert!(status.text() == "here" && !notice.visible());
        pump(&mut app, cx, at2, [FromWorker::Lost, backoff(now + 112)]);
        assert_eq!(guest_status(&app), (Presence::Away, status.text()));
        assert!(status.text().starts_with("away · last seen ") && !link.visible());

        app.switch_town(cx, private_town);
        assert!(!app.model.as_ref().unwrap().state.active_member(&guest));
        pump(&mut app, cx, at3, [reconnect(now + 112), phase(Status::Reconnecting), phase(Status::Securing), phase(Status::Proving)]);
        let before = updates(&app);
        assert_eq!(pump(&mut app, cx, at3, [FromWorker::PeerVersion(VERSION + 1)]), (false, false, false, true));
        assert_eq!(updates(&app), before, "an off-town version notice leaves the open town's row and timer alone");
        assert!(notice.visible() && notice_text.text().contains("Your copy of Makepad Town is older than "));
        pump(&mut app, cx, at3, [FromWorker::Refused("refused".into()), backoff(now + 129)]);
        app.switch_town(cx, shared);
        draw_home(&mut app, cx);
        home_action(&mut app, cx, member_chip(cx, MemberId::Computer(guest)));

        click(&mut app, cx, ids!(remove_member));
        assert!(line.visible() && line.text() == format!("Removing {guest_name} — that device can't rejoin this town.") && undo.visible() && !remove.visible(),
            "the pending action shows the member's name and Undo");
        assert!(app.model.as_ref().unwrap().state.active_member(&guest), "Remove leaves membership intact during Undo");
        click(&mut app, cx, ids!(undo_remove));
        assert!(remove.visible() && !undo.visible() && app.removing.is_none() && app.model.as_ref().unwrap().state.active_member(&guest),
            "Undo restores the actions and keeps the member");
        click(&mut app, cx, ids!(remove_member));
        let timer_id = app.removing.as_ref().unwrap().timer.0;
        app.handle_event(cx, &Event::Timer(event::TimerEvent { timer_id, time: None }));
        assert!(!app.model.as_ref().unwrap().state.active_member(&guest), "expiry writes the removal");
        assert!(app.removing.is_none() && !content.widget(cx, ids!(member_card)).visible(), "expiry closes the detail");

        // Keep two copies of the same three-device town so both Remove and Leave use real rights.
        use makepad_town_core::event::{Actor, Content, Draft, Event as TownEvent, Member, Via};
        use makepad_town_core::log::Log;
        let founder_dir = dir.join("founder");
        let joiner_dir = dir.join("joiner");
        let founder = Model::open(&founder_dir).unwrap().0;
        let joiner = Model::open(&joiner_dir).unwrap().0;
        let town = founder.open_id();
        let ana = founder.device_key();
        let ben = joiner.device_key();
        let kit = DeviceKey::generate().unwrap();
        let path = founder.home.town_log_path(&town);
        founder.home.set_open_town(&town).unwrap();
        let (other_town, genesis) = TownEvent::found(&ana, 2, 0, 0, [7; 16]).unwrap();
        Log::found(&founder.home.town_log_path(&other_town), genesis).unwrap();
        drop(founder);
        drop(joiner);
        let mut log = Log::open(&path).unwrap().0;
        let mut add = |key: &DeviceKey, content: Content| {
            let bytes = content.encode();
            let event = TownEvent::create(key, town, Draft {
                kind: content.kind(), kind_version: content.version(), critical: matches!(content, Content::Member(_)),
                actor: Actor::Human, via: Via::Ui, wall_ms: wall_ms(), parents: log.parents_for(&key.public()), content: &bytes,
            }, [0; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            id
        };
        add(&ana, Content::Profile { name: "Ana".into(), colour: 0 });
        for (device, name) in [(&*ben, "Ben"), (&kit, "Kit")] {
            let invite = add(&ana, Content::Member(Member::Invite { subject: device.public(), role: 0 }));
            add(device, Content::Member(Member::Accept { entry: invite }));
            add(device, Content::Profile { name: name.into(), colour: 0 });
        }
        drop(log);
        let joiner_home = makepad_town_core::home::Home::open(&joiner_dir).unwrap();
        std::fs::copy(&path, joiner_home.town_log_path(&town)).unwrap();
        joiner_home.set_open_town(&town).unwrap();
        drop(joiner_home);
        let founder = Model::open(&founder_dir).unwrap().0;
        let mut joiner = Model::open(&joiner_dir).unwrap().0;
        let founder_key = founder.me();
        let joiner_key = joiner.me();
        let others: Vec<_> = founder.state.active_members().filter(|k| *k != founder_key).collect();
        assert_eq!(others.len(), 2, "the founder has two active peers");
        let (first_peer, second_peer) = (others[0], others[1]);
        assert_ne!(first_peer, second_peer);
        let old_target = founder.state.active_members().find(|k| *k != founder_key);
        assert_eq!(old_target, Some(first_peer), "the old removal targeted the first peer");
        assert_ne!(old_target, Some(second_peer), "this card must exercise a different target");

        app.model = Some(founder);
        app.refresh(cx);
        draw_home(&mut app, cx);
        let second = MemberId::Computer(second_peer);
        let second_name = app.view.members.seats.iter().find(|s| s.id == second).unwrap().name.clone();
        let open_card = |app: &mut App, cx: &mut Cx, member| {
            let chip = member_chip(cx, member);
            home_action(app, cx, chip);
        };
        let start_removal = |app: &mut App, cx: &mut Cx| {
            click(app, cx, ids!(remove_member));
            app.removing.as_ref().expect("the card offers Undo before removal").timer.0
        };
        let fire_timer = |app: &mut App, cx: &mut Cx, timer_id| {
            app.handle_event(cx, &Event::Timer(event::TimerEvent { timer_id, time: None }));
        };
        let active = |app: &App, key| app.model.as_ref().unwrap().state.active_member(&key);
        open_card(&mut app, cx, second);
        assert!(remove.visible() && remove.text() == "Remove", "the second peer's card offers Remove");
        start_removal(&mut app, cx);
        assert!(line.visible() && line.text() == format!("Removing {second_name} — that device can't rejoin this town.")
            && undo.visible() && !remove.visible(), "the pending removal names the selected peer and offers Undo");
        assert!(active(&app, first_peer) && active(&app, second_peer), "both peers remain during Undo");
        click(&mut app, cx, ids!(undo_remove));
        assert!(app.removing.is_none() && remove.visible() && !undo.visible(), "Undo restores the card actions");
        assert!(active(&app, first_peer) && active(&app, second_peer), "Undo keeps both peers");

        let timer_id = start_removal(&mut app, cx);
        app.switch_town(cx, other_town);
        assert!(app.removing.is_none() && app.view.share_error.is_none(), "switching towns silently cancels removal");
        fire_timer(&mut app, cx, timer_id);
        assert!(app.view.share_error.is_none(), "a cancelled timer is ignored in the other town");
        app.switch_town(cx, town);
        assert!(active(&app, first_peer) && active(&app, second_peer), "switching towns keeps both peers");

        draw_home(&mut app, cx);
        open_card(&mut app, cx, second);
        start_removal(&mut app, cx);
        app.view.page = Page::Project;
        app.update_page(cx);
        assert!(app.removing.is_none() && active(&app, second_peer), "leaving Home cancels the pending removal");
        app.view.page = Page::Home;
        app.update_page(cx);
        assert!(card.visible() && app.view.selected_member == Some(second) && remove.visible() && !undo.visible(),
            "returning Home restores the selected peer's card with Remove available");
        let timer_id = start_removal(&mut app, cx);
        fire_timer(&mut app, cx, timer_id);
        assert!(!active(&app, second_peer) && active(&app, first_peer) && active(&app, founder_key),
            "expiry removes only the selected second peer");
        assert!(app.removing.is_none() && !card.visible() && app.view.share_error.is_none(), "expiry closes the card cleanly");

        std::mem::swap(app.model.as_mut().unwrap(), &mut joiner);
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::You);
        assert!(remove.visible() && remove.text() == "Leave", "a joiner can leave the three-device town");
        start_removal(&mut app, cx);
        assert!(line.visible() && line.text() == "Leaving this town — this device can't rejoin." && undo.visible(),
            "Leave explains the consequence while Undo is available");
        click(&mut app, cx, ids!(undo_remove));
        assert!(app.removing.is_none() && remove.visible() && active(&app, joiner_key), "Undo keeps this device in the town");
        let timer_id = start_removal(&mut app, cx);
        fire_timer(&mut app, cx, timer_id);
        assert!(!active(&app, joiner_key) && active(&app, founder_key) && active(&app, kit.public()),
            "Leave removes only the joiner's own device");

        std::mem::swap(app.model.as_mut().unwrap(), &mut joiner);
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Computer(first_peer));
        let timer_id = start_removal(&mut app, cx);
        app.handle_event(cx, &Event::Shutdown);
        assert!(app.removing.is_none() && active(&app, first_peer), "shutdown cancels the pending removal");
        fire_timer(&mut app, cx, timer_id);
        assert!(active(&app, first_peer), "a stale timer after shutdown cannot remove the peer");
        drop(joiner);
        drop(app);
        drop(peer);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn carriers_attach_all_compatible_peers_and_switch_preferred_peer_only_when_idle() {
        use makepad_create::call::{CallEnv, Message, Phase};
        use makepad_town_core::link::VERSION;
        use std::sync::Arc;

        fn exchange(a: &mut Model, b: &mut Model) {
            loop {
                let (out, incoming) = (a.pull_sync(b.me()), b.pull_sync(a.me()));
                if out.is_none() && incoming.is_none() { break }
                if let Some(bytes) = out { b.receive_from(a.me(), &bytes).unwrap(); }
                if let Some(bytes) = incoming { a.receive_from(b.me(), &bytes).unwrap(); }
            }
        }

        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        let old_peer = DeviceKey::generate().unwrap().public();
        let dir = std::env::temp_dir().join(format!("town-call-carriers-{}", makepad_town_core::hex(&old_peer)));
        let mut model = Model::open(&dir.join("ana")).unwrap().0;
        let [(ben, town_a), (kit, town_b)] = ["Ben", "Kit"].map(|name| {
            let mut peer = Model::open(&dir.join(name)).unwrap().0;
            let key = peer.me();
            model.pin_linked(&key, VERSION, name).unwrap();
            peer.pin_linked(&model.me(), VERSION, "Ana").unwrap();
            model.link_proved(&key, VERSION, name, None).unwrap();
            peer.link_proved(&model.me(), VERSION, "Ana", None).unwrap();
            exchange(&mut model, &mut peer);
            peer.offer_town(true).unwrap();
            exchange(&mut model, &mut peer);
            let prompt = model.offer_prompt().unwrap();
            assert!(model.answer_prompt(prompt, true));
            exchange(&mut model, &mut peer);
            assert_eq!(model.state_for(key), ShareState::Shared(prompt.town));
            (key, prompt.town)
        });
        model.open_town(town_a).unwrap();
        model.pin_linked(&old_peer, VERSION, "Old").unwrap();
        model.link_proved(&old_peer, VERSION, "Old", None).unwrap();
        assert!(!model.sync_compatible(old_peer));
        app.model = Some(model);
        app.call.configure(CallEnv {
            files: Default::default(), devices_path: dir.join("devices"), voice: makepad_call::driver::VoiceSource::Off,
            signal: Arc::new(|| {}), spawn: Arc::new(|_, _| panic!("a ring needs no media worker")),
            jobs: Arc::new(|_, _| panic!("a ring needs no audio settings job")),
        });
        let attempt = |peer, generation| Attempt { carrier: Carrier::Peer(peer), generation };
        let (ben1, ben2, kit1) = (attempt(ben, 1), attempt(ben, 2), attempt(kit, 1));
        app.carriers.up = BTreeMap::from([(ben1, "Ben".into()), (kit1, "Kit".into()), (attempt(old_peer, 1), "Old".into())]);
        app.sync_call_carriers(cx);
        assert_eq!(app.carriers.calls, BTreeMap::from([(ben, ben1), (kit, kit1)]));
        assert_eq!(app.call.linked_peers().collect::<BTreeSet<_>>(), BTreeSet::from([ben, kit]));
        assert_eq!(app.call.active_peer(), Some(ben), "prefer the peer in the open town");
        assert_eq!(app.link_queue.len(), 2);
        let ports: BTreeMap<_, _> = app.link_queue.drain(..).map(|request| match request {
            (Some(at), ToWorker::Call(port)) => (at, port),
            _ => panic!("expected only initial attachments"),
        }).collect();
        assert_eq!(ports.keys().copied().collect::<BTreeSet<_>>(), BTreeSet::from([ben1, kit1]));
        let epoch = |app: &App, peer| app.call.peer_epochs().find(|&(key, _)| key == peer).unwrap().1;
        let kit_epoch = epoch(&app, kit);

        app.carriers.up.remove(&ben1);
        app.carriers.up.insert(ben2, "Ben".into());
        app.sync_call_carriers(cx);
        assert_eq!(epoch(&app, kit), kit_epoch, "reconnecting Ben preserves Kit's link");
        assert_eq!(app.carriers.calls, BTreeMap::from([(ben, ben2), (kit, kit1)]));
        assert!(matches!(app.link_queue.pop_front(), Some((Some(at), ToWorker::EndCall)) if at == ben1));
        let Some((Some(at), ToWorker::Call(_ben_port))) = app.link_queue.pop_front() else { panic!("missing replacement port") };
        assert_eq!(at, ben2);
        assert!(app.link_queue.is_empty());

        app.model.as_mut().unwrap().open_town(town_b).unwrap();
        app.sync_call_carriers(cx);
        assert_eq!(app.call.active_peer(), Some(kit), "an idle switch follows the open town");
        assert_eq!(app.call.linked_peers().count(), 2);
        assert!(app.link_queue.is_empty());

        ports[&kit1].messages_in.try_send(Message::Ring {
            call_id: [3; 16], secret: [4; 32], video: false, ext: Default::default(),
        }.encode()).unwrap();
        app.call.poll(cx);
        assert_eq!(app.call.view().phase, Phase::Ringing);
        app.model.as_mut().unwrap().open_town(town_a).unwrap();
        app.sync_call_carriers(cx);
        assert_eq!(app.call.active_peer(), Some(kit), "the ringing call stays with Kit");
        assert_eq!(app.call.linked_peers().collect::<BTreeSet<_>>(), BTreeSet::from([ben, kit]));
        assert!(app.link_queue.is_empty());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn share_copy_uses_names_without_assigning_pronouns() {
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
