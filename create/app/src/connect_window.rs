//! The town page's side of the App: it starts and stops the link worker, hands it the words,
//! and moves sync messages between the worker and the model. The UI thread never waits on the
//! worker; a full queue keeps its commands for the next frame.

use makepad_town_core::key::PublicKey;
use crate::App;
use makepad_town_core::hub::{Attempt, Carrier};
use std::collections::{BTreeMap, BTreeSet};
use makepad_create::link_worker::{
    ConnectView, DeviceLink, FromWorker, Known, LinkWorker, PairWords, SendError, Status, ToWorker, WordsRejected,
    WORDS_OUT_OF_DATE,
};
use makepad_create::model::{wall_ms, Action, AddComputer, Model, NoticeAction, Origin, PairSide, ReinstatedDevice, ShareState, TownNotice};
use makepad_town_core::event::Id;
use makepad_create::{consent, pair_failure, ADDRESS_FAILED, DECLINED, PAIR_FAILED};
use makepad_create::lists::{clear_member_tip, HomeAction, Live, MemberDeviceRow, Node, Page, Pane};
use makepad_town_core::queue::Credential;
use makepad_create::view::{cascade_question, removing_seat_line, seat_label, tool_command, MemberId, MemberSeat,
    Presence, Seat, SeatDevice, TownMembers};
use makepad_widgets::*;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HomeStep {
    #[default] Overview,
    Ai(Id),
    Pair(PairSide),
    AddMenu(Id),
    Card { town: Id, member: MemberId },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShownDevice {
    town: Id,
    member: MemberId,
    device: PublicKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ShownRemove {
    Leave { town: Id, device: PublicKey },
    Seat { town: Id, seat: Id, devices: Vec<PublicKey> },
}

impl ShownRemove {
    fn for_card(model: &Model, row: &MemberSeat) -> Option<Self> {
        let town = model.open_id();
        match row.id {
            MemberId::You => Some(Self::Leave { town, device: model.me() }),
            MemberId::Seat(seat) => Some(Self::Seat { town, seat, devices: row.devices.iter().map(|d| d.key).collect() }),
            MemberId::YourAi | MemberId::Agent(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShownCascade {
    town: Id,
    seat: Id,
    generation: u64,
}

pub(super) struct ConfirmCascade {
    pub(super) town: Id,
    pub(super) member: MemberId,
    seat: Id,
    target_name: String,
    seat_devices: Vec<PublicKey>,
    cascade_seats: Vec<Id>,
    cascade_names: Vec<String>,
    generation: u64,
}

impl ConfirmCascade {
    fn shown(&self) -> ShownCascade {
        ShownCascade { town: self.town, seat: self.seat, generation: self.generation }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RemoveTarget {
    Device(PublicKey),
    Seat {
        seat: Id,
        also_added: bool,
        /// Active devices of this seat previewed when Undo started.
        expected_devices: Vec<PublicKey>,
        /// Cascade seats previewed when Undo started, checked for a cascading removal.
        expected_cascade_seats: Vec<Id>,
    },
    Leave,
}

pub(super) struct Removing {
    pub(super) town: Id,
    member: MemberId,
    target: RemoveTarget,
    line: String,
    pub(super) timer: Timer,
}

#[derive(Default)]
pub(super) struct ForkReinstated {
    key: Option<(Id, PublicKey, usize)>,
    devices: Vec<ReinstatedDevice>,
}

impl ForkReinstated {
    pub(super) fn clear(&mut self) {
        self.key = None;
        self.devices.clear();
    }
}

fn member_for_device(model: &Model, device: &PublicKey) -> Option<MemberId> {
    let seat = model.state.seat_of(device)?;
    Some(if model.state.seat_of(&model.me()) == Some(seat) { MemberId::You } else { MemberId::Seat(seat) })
}

fn single_peer_device(seat: &MemberSeat) -> Option<&SeatDevice> {
    match seat.devices.as_slice() {
        [device] if matches!(seat.id, MemberId::Seat(_)) => Some(device),
        _ => None,
    }
}

fn live_seat_devices(model: &Model, seat: Id) -> Vec<PublicKey> {
    model.seats_in_order().into_iter().find(|s| s.seat == seat).map(|s| s.devices).unwrap_or_default()
}

fn seat_display_name(model: &Model, members: &TownMembers, seat: Id) -> String {
    members.seats.iter().find(|s| s.id == MemberId::Seat(seat)).map(|s| s.name.clone())
        .unwrap_or_else(|| seat_label(model, &live_seat_devices(model, seat)))
}

fn only_previewed_devices(model: &Model, seat: Id, expected: &[PublicKey]) -> bool {
    model.state.active_members().filter(|d| model.state.seat_of(d) == Some(seat)).all(|d| expected.contains(&d))
}

fn shows_device_rows(seat: &MemberSeat) -> bool {
    seat.devices.len() > 1 || (seat.id == MemberId::You && seat.devices.iter().any(|d| !d.is_me))
}

fn device_detail(model: &Model, device: &SeatDevice) -> String {
    if device.is_me { return device.status.clone() }
    let mut detail = device.status.clone();
    use makepad_town_core::event::Service;
    let services = model.state.provides(&device.key);
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
    detail
}

pub(super) fn card_pressed<T: ActionTrait + PartialEq>(button: ButtonRef, actions: &Actions, shown: &T) -> bool {
    actions.iter().filter_map(|a| a.as_widget_action()).any(|a| a.widget_uid == button.widget_uid()
        && matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))
        && a.data.as_ref().and_then(|d| d.downcast_ref::<T>()) == Some(shown))
}

fn can_connect_device(
    model: &Model,
    link: &ConnectView,
    carriers: &Carriers,
    device: &SeatDevice,
    wanted_towns: &mut Option<BTreeSet<(PublicKey, Id)>>,
) -> bool {
    if device.is_me
        || device.presence != Presence::Away
        || link.first_contact
        || !model.state.active_member(&model.me())
        || !model.state.active_member(&device.key)
        || model.linked_with(&device.key)
        || model.peer_update(&device.key).is_some()
        || carriers.wants_device(&device.key)
        || !matches!(link.device_link(&device.key, 0), DeviceLink::Offline { .. })
        || carriers.revision == Some((model.me(), model.wanted_revision()))
        || model.known_device(&device.key).is_none()
    {
        return false;
    }
    let towns = wanted_towns.get_or_insert_with(|| model.wanted().1);
    towns.contains(&(device.key, model.open_id()))
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
    pub(super) shown_open_town: Option<(Id, Vec<PublicKey>)>,
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

    pub(super) fn home_visible(&self) -> bool {
        self.view.page == Page::Home && (!self.view.narrow() || self.view.pane == Pane::Page)
    }

    pub(super) fn active_fork_notice(&mut self) -> Option<(ReinstatedDevice, usize)> {
        if self.removing.is_some() { return None }
        let Some(model) = &self.model else {
            self.fork_reinstated.clear();
            return None
        };
        let town = model.open_id();
        let key = (town, model.me(), model.open_event_count());
        if self.fork_reinstated.key != Some(key) {
            self.fork_reinstated.devices = model.reinstated_by_fork();
            self.fork_reinstated.key = Some(key);
        }
        let mut visible = self.fork_reinstated.devices.iter()
            .filter(|item| !self.dismissed_forks.contains(&(town, item.removal_event)));
        let first = visible.next()?.clone();
        Some((first, visible.count()))
    }

    pub(super) fn fork_town_notice(&mut self) -> Option<TownNotice> {
        let active = self.active_fork_notice();
        let Some(model) = &self.model else { return None };
        let base = model.home_notice_without_split(active.as_ref().map(|(item, _)| &item.forked_device));
        let Some((item, extra)) = active else { return base };
        let town = model.open_id();
        let forked_name = model.device_label(&item.forked_device);
        let target_name = model.device_label(&item.target_device);
        let mut line = format!("{forked_name}'s history split. Its removal of {target_name} no longer counts.");
        if extra > 0 {
            line.push_str(&format!(" ({extra} more)"));
        }
        let (details, replaces_read_only) = if let Some(base) = base {
            if !base.line.is_empty() {
                line.push('\n');
                line.push_str(&base.line);
            }
            (base.details, base.replaces_read_only)
        } else {
            (None, false)
        };
        let dismiss = NoticeAction::DismissFork { town, removal_event: item.removal_event };
        let (offer, hide) = if item.target_device != model.me() && model.can_remove_device(&item.target_device) {
            let action = NoticeAction::ReRemove { town, device: item.target_device, removal_event: item.removal_event };
            (Some((format!("Remove {target_name} again"), action)), Some((format!("Keep {target_name}"), dismiss)))
        } else {
            (None, Some(("OK".into(), dismiss)))
        };
        Some(TownNotice { line, offer, hide, details, replaces_read_only })
    }

    fn fork_notice_remove_target(&mut self) -> Option<PublicKey> {
        let (item, _) = self.active_fork_notice()?;
        let model = self.model.as_ref()?;
        (item.target_device != model.me() && model.can_remove_device(&item.target_device)).then_some(item.target_device)
    }

    fn open_member_card(&mut self, cx: &mut Cx, town: Id, member: MemberId) {
        self.confirm_cascade = None;
        self.home_step = HomeStep::Card { town, member };
        let ai_name = self.ui.text_input(cx, ids!(ai_name_input));
        let area = ai_name.area();
        if !area.is_empty() && cx.has_key_focus(area) { cx.set_key_focus(Area::Empty); }
        if let MemberId::Agent(id) = member {
            if let Some(model) = self.model.as_ref().filter(|m| m.can_rename_agent(id)) {
                ai_name.set_text(cx, &model.agent_custom_name(id).unwrap_or_default());
            }
        }
        if member == MemberId::You {
            self.hub_probe(cx);
            if let Some(model) = &self.model {
                match model.home.computer_label() {
                    Ok(label) => self.ui.text_input(cx, ids!(computer_label)).set_text(cx, &label),
                    Err(err) => self.view.share_error = Some(err.to_string()),
                }
            }
        }
    }

    fn start_device_removal(&mut self, cx: &mut Cx, town: Id, member: MemberId, device: PublicKey, name: &str) {
        self.removing = Some(Removing { town, member, target: RemoveTarget::Device(device),
            line: format!("Removing {name} — that device can't rejoin this town."), timer: cx.start_timeout(5.0) });
    }

    pub(super) fn town_notice_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let Some(action) = TownNotice::clicked(actions).cloned() else { return };
        match &action {
            NoticeAction::ReRemove { town, device, removal_event } => {
                if !self.home_visible() || self.removing.is_some() { return }
                let Some((active, _)) = self.active_fork_notice() else { return };
                let Some(model) = &self.model else { return };
                if model.open_id() != *town || active.target_device != *device || active.removal_event != *removal_event { return }
                if *device == model.me() || !model.can_remove_device(device) { return }
                let Some(member) = member_for_device(model, device) else { return };
                let name = self.view.members.seats.iter().find(|s| s.id == member)
                    .and_then(|s| s.devices.iter().find(|d| d.key == *device))
                    .map(|d| d.name.clone()).unwrap_or_else(|| model.device_label(device).to_owned());
                self.view.share_error = None;
                self.open_member_card(cx, *town, member);
                self.start_device_removal(cx, *town, member, *device, &name);
                self.update_town_notice(cx);
                self.update_connect(cx);
                return
            }
            NoticeAction::DismissFork { town, removal_event } => {
                if !self.home_visible() { return }
                let Some((active, _)) = self.active_fork_notice() else { return };
                if self.model.as_ref().map(Model::open_id) != Some(*town) || active.removal_event != *removal_event { return }
                self.confirm_cascade = None;
                self.dismissed_forks.insert((*town, *removal_event));
                self.update_town_notice(cx);
                self.update_member_card(cx);
                self.ui.widget(cx, ids!(home_page)).redraw(cx);
                return
            }
            NoticeAction::Answer { .. } | NoticeAction::Offer { .. } => {}
        }
        let Some(model) = &mut self.model else { return };
        match model.act_on_notice(&action) {
            Ok(false) => return,
            Ok(true) => self.view.share_error = None,
            Err(error) => self.view.share_error = Some(error),
        }
        self.confirm_cascade = None;
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
                    if matches!(self.home_step, HomeStep::Card { member: open, .. } if open == member) {
                        self.home_step = self.pending_home_step(id);
                    } else {
                        self.open_member_card(cx, id, member);
                    }
                }
                _ => continue,
            }
            self.confirm_cascade = None;
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
        if let Some(cascade) = &self.confirm_cascade {
            let shown = Some(cascade.shown());
            let prompt = self.ui.widget(cx, ids!(cascade_prompt));
            if card_pressed(prompt.button(cx, ids!(cascade_cancel)), actions, &shown) {
                self.confirm_cascade = None;
                return true
            }
            let only = card_pressed(prompt.button(cx, ids!(cascade_only)), actions, &shown);
            let all = card_pressed(prompt.button(cx, ids!(cascade_all)), actions, &shown);
            if (!only && !all) || !self.home_visible() { return false }
            let Some(model) = &self.model else { return false };
            let ConfirmCascade { town, member, seat, .. } = *cascade;
            if model.open_id() != town || self.home_step != (HomeStep::Card { town, member }) || !model.can_remove_seat(seat) {
                return false
            }
            let devices = live_seat_devices(model, seat);
            if devices.is_empty() || devices.iter().any(|d| !cascade.seat_devices.contains(d)) {
                self.confirm_cascade = None;
                return true
            }
            let also_added = !only;
            let (expected_cascade_seats, names) = if also_added {
                let current = model.removal_cascade(seat);
                let names = current.seats.iter().map(|&s| seat_display_name(model, &self.view.members, s)).collect::<Vec<_>>();
                if current.seats.iter().any(|s| !cascade.cascade_seats.contains(s)) {
                    self.cascade_generation = self.cascade_generation.wrapping_add(1);
                    let cascade = self.confirm_cascade.as_mut().unwrap();
                    cascade.seat_devices = devices;
                    cascade.cascade_seats = current.seats;
                    cascade.cascade_names = names;
                    cascade.generation = self.cascade_generation;
                    return true
                }
                (current.seats, names)
            } else {
                (Vec::new(), Vec::new())
            };
            let line = removing_seat_line(&cascade.target_name, devices.len(), &names);
            self.removing = Some(Removing { town, member, line, timer: cx.start_timeout(5.0),
                target: RemoveTarget::Seat { seat, also_added, expected_devices: devices, expected_cascade_seats } });
            self.confirm_cascade = None;
            self.view.share_error = None;
            return true
        }
        let HomeStep::Card { town, member } = self.home_step else { return false };
        if self.model.as_ref().map(Model::open_id) != Some(town) { return false }
        let device_action = actions.iter().filter_map(|a| a.as_widget_action()).find_map(|click| {
            matches!(click.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))
                .then(|| click.data.as_ref().and_then(|d| d.downcast_ref::<HomeAction>()).copied())
                .flatten()
                .filter(|a| matches!(a, HomeAction::DeviceConnect { .. } | HomeAction::DeviceRemove { .. }))
        });
        let row = self.view.members.seats.iter().find(|s| s.id == member);
        let shown_connect = row.and_then(single_peer_device).map(|d| ShownDevice { town, member, device: d.key });
        let remove_button = self.ui.button(cx, ids!(remove_member));
        let shown_remove = row.zip(self.model.as_ref()).and_then(|(seat, model)| ShownRemove::for_card(model, seat));
        let pressed_remove = shown_remove.is_some() && card_pressed(remove_button, actions, &shown_remove);
        let pressed_connect = shown_connect.is_some()
            && card_pressed(self.ui.button(cx, ids!(device_link)), actions, &shown_connect);
        let notice_remove_target = if (pressed_remove && matches!(member, MemberId::Seat(_)))
            || matches!(device_action, Some(HomeAction::DeviceRemove { .. }))
        {
            self.fork_notice_remove_target()
        } else {
            None
        };
        let home_visible = self.home_visible();
        let Some(model) = &mut self.model else { return false };
        let Some(seat) = self.view.members.seats.iter().find(|s| s.id == member) else { return false };
        let home_step = self.home_step;
        let pressed = |path| card_pressed(self.ui.button(cx, path), actions, &home_step);
        let (target, remove) = match device_action {
            Some(HomeAction::DeviceConnect { town: id, member: m, device } | HomeAction::DeviceRemove { town: id, member: m, device }) => {
                if id != town || m != member || !shows_device_rows(seat) { return false }
                (seat.devices.iter().find(|d| d.key == device), matches!(device_action, Some(HomeAction::DeviceRemove { .. })))
            }
            _ if pressed_remove && member == MemberId::You => {
                if !home_visible || !model.can_leave_town() { return false }
                self.removing = Some(Removing { town, member, target: RemoveTarget::Leave,
                    line: "Leaving this town — this device can't rejoin.".into(), timer: cx.start_timeout(5.0) });
                self.view.share_error = None;
                return true
            }
            _ if pressed_remove => {
                let Some(ShownRemove::Seat { seat: seat_id, devices: shown_devices, .. }) = shown_remove else { return false };
                if !home_visible || !model.can_remove_seat(seat_id)
                    || single_peer_device(seat).is_some_and(|d| Some(d.key) == notice_remove_target) { return false }
                let devices = live_seat_devices(model, seat_id);
                if devices.is_empty() || devices != shown_devices { return false }
                let impact = model.removal_cascade(seat_id);
                if impact.seats.is_empty() {
                    let line = removing_seat_line(&seat.name, devices.len(), &[]);
                    self.removing = Some(Removing { town, member, line, timer: cx.start_timeout(5.0),
                        target: RemoveTarget::Seat { seat: seat_id, also_added: false,
                            expected_devices: devices, expected_cascade_seats: Vec::new() } });
                } else {
                    let cascade_names = impact.seats.iter().map(|&s| seat_display_name(model, &self.view.members, s)).collect();
                    self.cascade_generation = self.cascade_generation.wrapping_add(1);
                    self.confirm_cascade = Some(ConfirmCascade { town, member, seat: seat_id, target_name: seat.name.clone(),
                        seat_devices: devices, cascade_seats: impact.seats, cascade_names, generation: self.cascade_generation });
                }
                self.view.share_error = None;
                return true
            }
            _ if pressed_connect => (single_peer_device(seat), false),
            _ => (None, false),
        };
        if let Some(device) = target {
            let key = device.key;
            if remove {
                if !home_visible || device.is_me || key == model.me() || !model.can_remove_device(&key)
                    || notice_remove_target == Some(key) { return false }
                let name = device.name.clone();
                self.view.share_error = None;
                self.start_device_removal(cx, town, member, key, &name);
            } else {
                if !can_connect_device(model, &self.view.connect, &self.carriers, device, &mut None) { return false }
                self.connect_resume(cx, key);
            }
        } else if device_action.is_some() {
            return false
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
        } else if let MemberId::Agent(agent) = member {
            if pressed(ids!(remove_ai)) {
                if seat.seat != Seat::YourAi || !self.view.workspace.writable { return false }
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
            } else if model.can_rename_agent(agent) {
                let field = self.ui.text_input(cx, ids!(ai_name_input));
                if field.escaped(actions) {
                    self.home_step = self.pending_home_step(town);
                    return true
                }
                let Some((name, _)) = field.returned(actions) else { return false };
                match model.rename_agent(agent, &name) {
                    Ok(()) => {
                        self.view.share_error = None;
                        self.home_step = self.pending_home_step(town);
                    }
                    Err(why) => self.view.share_error = Some(why),
                }
            } else { return false }
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
        if self.confirm_cascade.as_ref().is_some_and(|c| c.town != town) { self.confirm_cascade = None; }
        if self.removing.as_ref().is_some_and(|r| r.town != town) { self.cancel_removal(cx); }
        if let Some(model) = &mut self.model {
            if let Err(e) = model.open_town(town) { log!("town: opening a town: {e}"); }
        }
        if self.sync_call_carriers(cx) { self.show_call(cx); }
        self.refresh(cx);
    }

    pub(super) fn cancel_removal(&mut self, cx: &mut Cx) {
        let Some(Removing { timer, .. }) = self.removing.take() else { return };
        cx.stop_timer(timer);
    }

    pub(super) fn finish_removal(&mut self, cx: &mut Cx) {
        let Some(Removing { town, member, target, timer, .. }) = self.removing.take() else { return };
        cx.stop_timer(timer);
        if !self.home_visible() {
            return;
        }
        let Some(model) = &mut self.model else { return };
        self.view.share_error = match target {
            RemoveTarget::Leave if model.open_id() == town && model.can_leave_town() => model.leave_town().err(),
            RemoveTarget::Device(key) if model.open_id() == town && key != model.me() && model.can_remove_device(&key) =>
                model.remove_device(key, false).err(),
            RemoveTarget::Seat { seat, also_added, expected_devices, expected_cascade_seats }
                if model.open_id() == town && model.can_remove_seat(seat)
                    && only_previewed_devices(model, seat, &expected_devices)
                    && (!also_added || model.removal_cascade(seat).seats.iter().all(|s| expected_cascade_seats.contains(s))) =>
                model.remove_seat(seat, also_added).err(),
            _ => Some("The town changed before the removal could finish.".into()),
        };
        if self.sync_call_carriers(cx) { self.show_call(cx); }
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

    pub(crate) fn can_call_member_seat(&self, town: Id, seat: &MemberSeat) -> bool {
        let MemberId::Seat(seat_id) = seat.id else { return false };
        if self.removing.is_some() || self.confirm_cascade.is_some() || !self.view.call.can_call
            || self.call.view().phase != makepad_create::call::Phase::Idle {
            return false
        }
        let Some((call_town, peers)) = self.call.open_town() else { return false };
        if call_town != town || !peers.iter().any(|key| seat.has_device(key))
            || !self.call.can_call_seat(town, seat_id) {
            return false
        }
        let Some(model) = &self.model else { return false };
        model.open_id() == town && model.active_town_call(town).is_none()
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
        let keep_cascade = self.confirm_cascade.as_ref().is_some_and(|c| {
            removing.is_none() && Some(c.town) == town
                && self.home_step == (HomeStep::Card { town: c.town, member: c.member })
                && seat.is_some_and(|s| s.devices.iter().all(|d| c.seat_devices.contains(&d.key)))
                && self.model.as_ref().is_some_and(|m| m.can_remove_seat(c.seat) && only_previewed_devices(m, c.seat, &c.seat_devices))
        });
        if !keep_cascade { self.confirm_cascade = None; }
        if self.view.selected_member != selected {
            self.view.selected_member = selected;
            let members = self.ui.widget(cx, ids!(home_members));
            if let Some(member) = selected {
                clear_member_tip(cx, &members, member);
            }
            members.redraw(cx);
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
        let editable_ai = match selected {
            Some(MemberId::Agent(id)) if removing.is_none() && self.model.as_ref().is_some_and(|m| m.can_rename_agent(id)) => Some(id),
            _ => None,
        };
        let ai_name = card.text_input(cx, ids!(ai_name_input));
        let ai_backend = card.label(cx, ids!(ai_backend));
        if editable_ai.is_none() {
            ai_name.set_visible(cx, false);
            ai_backend.set_visible(cx, false);
            let area = ai_name.area();
            if !area.is_empty() && cx.has_key_focus(area) { cx.set_key_focus(Area::Empty); }
        }
        let device_list = card.widget(cx, ids!(seat_devices));
        let show_rows = removing.is_none() && self.confirm_cascade.is_none() && self.model.is_some() && seat.is_some_and(shows_device_rows);
        device_list.set_visible(cx, show_rows);
        if !show_rows && !self.view.member_devices.is_empty() {
            self.view.member_devices.clear();
            device_list.redraw(cx);
        }
        let prompt = card.widget(cx, ids!(cascade_prompt));
        let shown_cascade = self.confirm_cascade.as_ref().map(ConfirmCascade::shown);
        card.widget(cx, ids!(member_head)).set_visible(cx, shown_cascade.is_none());
        prompt.set_visible(cx, shown_cascade.is_some());
        for path in [ids!(cascade_all), ids!(cascade_only), ids!(cascade_cancel)] {
            prompt.button(cx, path).set_action_data(shown_cascade);
        }
        let watch = card.button(cx, ids!(member_watch));
        let open_call = card.button(cx, ids!(member_open_call));
        let call_btn = card.button(cx, ids!(member_call));
        if selected.is_none() || removing.is_some() || shown_cascade.is_some() {
            watch.set_action_data(());
            open_call.set_action_data(());
            call_btn.set_action_data(());
        }
        if selected.is_none() { return }
        let undo = card.button(cx, ids!(undo_remove));
        let line = card.label(cx, ids!(removing_line));
        line.set_visible(cx, removing.is_some());
        undo.set_visible(cx, removing.is_some());
        if let Some(removing) = removing {
            for path in [ids!(member_badge), ids!(member_name), ids!(computer_label), ids!(ai_name_input), ids!(ai_backend), ids!(member_status), ids!(hub_model),
                         ids!(member_watch), ids!(member_open_call), ids!(member_call), ids!(device_link), ids!(hub_offer), ids!(remove_ai), ids!(remove_member)] {
                card.widget(cx, path).set_visible(cx, false);
            }
            line.set_text(cx, &removing.line);
            undo.set_action_data(removing.timer.0);
            return
        }
        if let Some(cascade) = &self.confirm_cascade {
            let (question, all, only) = cascade_question(&cascade.target_name, &cascade.cascade_names);
            prompt.label(cx, ids!(cascade_question)).set_text(cx, &question);
            prompt.button(cx, ids!(cascade_all)).set_text(cx, &all);
            prompt.button(cx, ids!(cascade_only)).set_text(cx, &only);
            return
        }
        let notice_remove_target = if seat.is_some_and(|s| s.devices.iter().any(|d| !d.is_me)) {
            self.fork_notice_remove_target()
        } else { None };
        let Some(model) = &self.model else { return };
        let Some(seat) = self.view.members.seats.iter().find(|s| Some(s.id) == selected) else { return };
        let badge = card.widget(cx, ids!(member_badge));
        badge.set_visible(cx, true);
        makepad_create::lists::seat_icon(cx, &badge, seat.seat, &seat.name, seat.tone);
        let person = self.view.live_roster.town.is_none_or(|t| t == model.open_id())
            && matches!(seat.id, MemberId::You | MemberId::Seat(_));
        let share = person.then(|| self.view.live_roster.share_for_devices(&seat.devices)).flatten()
            .and_then(|(_, session, title)| self.view.call.share.offer(session).map(|offer|
                (session, title, matches!(offer.role, makepad_create::call::ShareRole::Sharer { .. }))));
        watch.set_visible(cx, share.is_some());
        if let Some((session, title, is_own)) = share {
            watch.set_text(cx, &if is_own { "Open share".to_owned() } else { format!("Watch {title}") });
            watch.set_action_data(Node::Live(Live::Share(session)));
        } else {
            watch.set_action_data(());
        }
        let show_call = person && self.view.has_live(Live::Call) && self.view.live_roster.has_call_for_devices(&seat.devices);
        open_call.set_visible(cx, show_call);
        if show_call { open_call.set_action_data(Node::Live(Live::Call)); }
        else { open_call.set_action_data(()); }
        let show_member_call = self.can_call_member_seat(model.open_id(), seat);
        call_btn.set_visible(cx, show_member_call);
        if show_member_call { call_btn.set_action_data(self.home_step); }
        else { call_btn.set_action_data(()); }
        let you = seat.id == MemberId::You;
        let name = card.label(cx, ids!(member_name));
        name.set_visible(cx, !you && editable_ai.is_none());
        name.set_text(cx, &seat.name);
        if let Some(id) = editable_ai {
            ai_name.set_visible(cx, true);
            let area = ai_name.area();
            if area.is_empty() || !cx.has_key_focus(area) {
                let custom = model.agent_custom_name(id).unwrap_or_default();
                if ai_name.text() != custom { ai_name.set_text(cx, &custom); }
            }
            let backend = model.agent_backend(id).unwrap_or_default();
            let backend = backend.trim();
            ai_backend.set_visible(cx, !backend.is_empty());
            if !backend.is_empty() { ai_backend.set_text(cx, &format!("· {backend}")); }
        }
        let status = card.label(cx, ids!(member_status));
        let single = single_peer_device(seat);
        let detail = single.map_or_else(|| seat.status.clone(), |d| device_detail(model, d));
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
        let mut wanted_towns = None;
        let can_remove = |d: &SeatDevice| !d.is_me && model.can_remove_device(&d.key) && notice_remove_target != Some(d.key);
        if show_rows {
            let rows: Vec<_> = seat.devices.iter().map(|d| MemberDeviceRow {
                key: d.key, name: d.name.clone(), detail: device_detail(model, d), presence: d.presence, is_me: d.is_me,
                can_connect: can_connect_device(model, &self.view.connect, &self.carriers, d, &mut wanted_towns),
                can_remove: can_remove(d),
            }).collect();
            if self.view.member_devices != rows {
                self.view.member_devices = rows;
                device_list.redraw(cx);
            }
        }
        let link = card.button(cx, ids!(device_link));
        link.set_visible(cx, single.is_some_and(|d| can_connect_device(model, &self.view.connect, &self.carriers, d, &mut wanted_towns)));
        card.button(cx, ids!(remove_ai)).set_visible(cx, matches!(seat.id, MemberId::Agent(_)) && seat.seat == Seat::YourAi && self.view.workspace.writable);
        let remove = card.button(cx, ids!(remove_member));
        let can_remove_top = match seat.id {
            MemberId::You => model.can_leave_town(),
            MemberId::Seat(id) => model.can_remove_seat(id) && single.is_none_or(|d| notice_remove_target != Some(d.key)),
            _ => false,
        };
        remove.set_visible(cx, can_remove_top);
        if you { remove.set_text(cx, "Leave"); }
        else if single.is_some() { remove.set_text(cx, "Remove"); }
        else if matches!(seat.id, MemberId::Seat(_)) { remove.set_text(cx, &format!("Remove {}", seat.name)); }
        for path in [ids!(hub_offer), ids!(remove_ai)] {
            card.button(cx, path).set_action_data(self.home_step);
        }
        let town = model.open_id();
        let peer_shown = single.map(|d| ShownDevice { town, member: seat.id, device: d.key });
        remove.set_action_data(ShownRemove::for_card(model, seat));
        link.set_action_data(peer_shown);
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
        self.confirm_cascade = None;
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
        if v.waiting_since.is_some() {
            if let Some(side) = self.pair_side_shown().filter(|_| v.failed.is_none() && v.blocked.is_none() && self.pair_step_visible()) {
                self.pair_form(cx, side).label(cx, ids!(pair_status)).set_text(cx, &v.wait_status(wall_ms() / 1000));
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

    fn sync_call_carriers(&mut self, cx: &mut Cx) -> bool {
        let Some(model) = &self.model else { return false };
        let mut changed = false;
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
            changed |= self.call.link_down(cx, peer);
            self.link_queue.push_back((Some(old_at), ToWorker::EndCall));
        }
        for (peer, at) in desired {
            if self.carriers.calls.contains_key(&peer) { continue }
            let name = &self.carriers.up[&at];
            if let Some(port) = self.call.link_up(cx, model.me(), peer, name) {
                changed = true;
                self.carriers.calls.insert(peer, at);
                self.link_queue.push_back((Some(at), ToWorker::Call(port)));
            }
        }
        for &peer in self.carriers.calls.keys() {
            changed |= self.call.set_peer_caps(peer, model.peer_media_caps(&peer));
        }
        let me = model.me();
        let mut towns = BTreeMap::new();
        let mut town_seats = BTreeMap::new();
        for town in model.towns() {
            let members: BTreeSet<_> = self.carriers.calls.keys().copied()
                .filter(|&peer| peer != me && model.peer_multiparty(&peer)
                    && model.verify_wire_town(town.id, Some(peer)).is_some()).collect();
            let seats = std::iter::once(me).chain(members.iter().copied()).filter_map(|peer|
                model.seat_in_town(town.id, &peer).map(|seat| (peer, seat))).collect();
            town_seats.insert(town.id, seats);
            if !members.is_empty() { towns.insert(town.id, members); }
        }
        self.call.set_town_peer_seats(town_seats);
        changed |= self.call.set_town_peers(cx, towns);
        let open = model.open_id();
        let peers = model.linked_town_peers(open).into_iter().filter_map(|peer| {
            let at = self.carriers.calls.get(&peer)?;
            Some((peer, self.carriers.up[at].clone()))
        }).collect();
        changed |= self.call.set_open_town(Some((open, peers)));
        changed
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
        let Some(model) = &self.model else { return };
        let Some(known) = model.known_device(&device) else { return };
        if !self.carriers.wants_device(&device) {
            self.view.retry_quiet.clear(&device);
        }
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
                if request == self.carriers.words_request && self.view.connect.first_contact {
                    self.carriers.words = at;
                    for (target, cmd) in &mut self.link_queue {
                        if matches!(cmd, ToWorker::PeerWords { .. }) { *target = at; }
                    }
                }
                continue;
            }
            if let FromWorker::Idle(epoch) = event {
                if self.carriers.wanted.is_empty() && self.link_queue.is_empty()
                    && self.link.as_ref().is_some_and(|w| w.configuration() == epoch) {
                    self.carriers.words = None;
                    self.view.connect.apply_attempt(None, &event, false);
                    self.view.retry_quiet = Default::default();
                    self.link = None;
                    if self.view.connect.first_contact && self.view.connect.failed.is_none() && self.view.connect.linked.is_none() {
                        log!("town: link worker went idle while pairing was open");
                        if let Some(model) = &mut self.model { model.cancel_pair_intent(); }
                        cx.stop_timer(std::mem::take(&mut self.pair_timer));
                        self.view.connect.waiting_since = None;
                        self.view.connect.failed = Some(WORDS_OUT_OF_DATE.into());
                        changed = true;
                    }
                    break;
                }
                continue;
            }
            let words = at.is_some_and(|at| at.carrier == Carrier::Words);
            let pending_rejection = matches!(event, FromWorker::WordsRejected(_)) && self.view.connect.waiting_since.is_some();
            if words && (!self.view.connect.first_contact || (at != self.carriers.words && !pending_rejection)) { continue }
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
            changed |= !quiet_retry && !matches!(event, FromWorker::Message(_) | FromWorker::BlobStatus
                | FromWorker::BlobWorker(_) | FromWorker::WordsRejected(WordsRejected::AlreadyPairing));
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
                FromWorker::WordsRejected(WordsRejected::StaleAttempt | WordsRejected::OlderInvite | WordsRejected::NotReady) => {
                    model.cancel_pair_intent();
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
                    if at.is_none() {
                        self.link = None;
                        self.carriers = Carriers::default();
                        self.link_queue.clear();
                        self.call.link_down_all(cx);
                        model.sync_stop();
                        self.view.connect.waiting_since = None;
                        if self.pair_timer.0 != 0 { cx.stop_timer(std::mem::take(&mut self.pair_timer)); }
                        break;
                    }
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
            if self.link_queue.iter().any(|(_, cmd)| matches!(cmd, ToWorker::PeerWords { .. })) {
                self.link_queue.retain(|(_, cmd)| !matches!(cmd, ToWorker::PeerWords { .. }));
                log!("town: pairing: no link worker for peer words");
                if let Some(model) = &mut self.model { model.cancel_pair_intent(); }
                cx.stop_timer(std::mem::take(&mut self.pair_timer));
                self.view.connect.waiting_since = None;
                self.view.connect.failed = Some(PAIR_FAILED.into());
                self.update_connect(cx);
            }
            let held = crate::update_hold_of(&self.call, &self.view, &self.recorder).is_some();
            let wake = self.model.as_mut().and_then(|m| { m.set_update_hold(held); m.sources_wake(std::time::Instant::now()) });
            self.link_wake(cx, wake);
            if self.model.as_mut().is_some_and(Model::take_update_notice_changed) { self.update_town_notice(cx); }
            return
        }
        let Some((refresh, mut changed, presence_changed)) = self.drain_link_notices(cx, |app| {
            use std::sync::mpsc::TryRecvError;
            if let Some(notice) = app.unread_link.take() { return Some(notice) }
            match app.link.as_ref()?.next_event() {
                Ok(notice) => Some(notice),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    app.link = None;
                    Some((None, FromWorker::Failed("the link worker stopped unexpectedly".into())))
                }
            }
        }) else { return };
        self.reconcile_carriers();
        if self.sync_call_carriers(cx) { self.show_call(cx); }
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
        while self.link.is_some() {
            let Some(index) = self.link_queue.iter().position(|(at, cmd)| match (at, cmd) {
                (Some(Attempt { carrier: Carrier::Peer(p), .. }), ToWorker::Send(_)) =>
                    self.model.as_ref().is_none_or(|m| m.blob_policy_ready(*p)),
                _ => true,
            }) else { break };
            let Some(command) = self.link_queue.remove(index) else { break };
            match self.link.as_ref().unwrap().send(command) {
                Ok(()) => {}
                Err(SendError::Full(command)) => {
                    self.link_queue.insert(index, command);
                    log!("town: the link's queue is full; the rest goes on the next frame");
                    let _ = cx.new_next_frame();
                    break;
                }
                Err(SendError::Disconnected(_)) => {
                    log!("town: link worker disconnected");
                    self.link = None;
                    self.carriers = Carriers::default();
                    self.link_queue.clear();
                    self.call.link_down_all(cx);
                    if let Some(model) = &mut self.model {
                        model.sync_stop();
                        model.cancel_pair_intent();
                    }
                    if self.view.connect.first_contact && self.view.connect.failed.is_none() {
                        cx.stop_timer(std::mem::take(&mut self.pair_timer));
                        self.view.connect.waiting_since = None;
                        self.view.connect.failed = Some(PAIR_FAILED.into());
                        changed = true;
                    }
                    break;
                }
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
pub(crate) mod tests {
    use super::*;
    use makepad_town_core::{event::Content, log::Log};
    use makepad_town_core::key::DeviceKey;
    use makepad_network::p2p::{discover::Hint, words::Invite};

    fn press(cx: &mut Cx, widget: WidgetRef) -> ActionsBuf {
        let request = a11y_tree::ActionRequest { req: 0, node: a11y_tree::NodeId(widget.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        cx.capture_actions(|cx| {
            widget.borrow_mut::<Button>().unwrap().handle_action(cx, &request, &mut Scope::empty());
        })
    }

    pub(crate) fn append(log: &mut Log, key: &DeviceKey, content: Content, fork_parent: Option<Id>) -> Id {
        use makepad_town_core::event::{Actor, Draft, Event as TownEvent, Via};
        let (parents, salt) = match fork_parent {
            Some(parent) => (vec![parent], [7; 16]),
            None => (log.parents_for(&key.public()), [0; 16]),
        };
        let bytes = content.encode();
        let event = TownEvent::create(key, log.town_id(), Draft {
            kind: content.kind(), kind_version: content.version(), critical: matches!(content, Content::Member(_)),
            actor: Actor::Human, via: Via::Ui, wall_ms: wall_ms(), parents, content: &bytes,
        }, salt).unwrap();
        let id = event.id;
        log.append(event).unwrap();
        id
    }

    #[test]
    fn orphaned_peer_words_and_idle_during_pairing_fail_visibly() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, { use mod.widgets.* View{ join_page := JoinPage{} } });
            WidgetRef::script_from_value(vm, root)
        });
        let key = DeviceKey::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("town-orphaned-words-{}", makepad_town_core::hex(&key.public())));
        app.model = Some(Model::open(&dir).unwrap().0);
        app.view.page = Page::Join;
        let own = Invite { nat: Hint::Easy, addr: "192.0.2.1:4100".parse().unwrap(), lan: None, commit: 1 };
        let peer = Invite { addr: "192.0.2.2:4200".parse().unwrap(), ..own.clone() };
        enum WorkerState { Missing, Idle, Disconnected }
        for (state, why, shown) in [
            (WorkerState::Missing, PAIR_FAILED, PAIR_FAILED),
            (WorkerState::Idle, WORDS_OUT_OF_DATE, WORDS_OUT_OF_DATE),
            (WorkerState::Disconnected, "the link worker stopped unexpectedly", PAIR_FAILED),
        ] {
            app.view.connect = ConnectView {
                first_contact: true, side: Some(PairSide::Join), own_words: Some(own.to_words().join(" ")),
                ..Default::default()
            };
            app.try_pair_draft(cx, &peer.to_words().join(" "));
            assert!(app.view.connect.waiting_since.is_some() && app.model.as_ref().unwrap().pairing_since().is_some());
            assert_ne!(app.pair_timer.0, 0);
            if matches!(state, WorkerState::Idle | WorkerState::Disconnected) {
                app.ensure_worker().unwrap();
                assert!(matches!(app.link_queue.pop_front(), Some((_, ToWorker::PeerWords { .. }))));
                assert!(app.link_queue.is_empty());
            }
            match state {
                WorkerState::Missing => {
                    assert!(app.link.is_none());
                    app.link_event(cx);
                }
                WorkerState::Idle => {
                    let mut notice = Some((None, FromWorker::Idle(0)));
                    let (refresh, changed, presence) = app.drain_link_notices(cx, |_| notice.take()).unwrap();
                    assert!(changed);
                    app.apply_link_updates(cx, refresh, changed, presence);
                }
                WorkerState::Disconnected => {
                    use std::sync::mpsc::TryRecvError;
                    app.view.connect.apply(&FromWorker::Phase(Status::Reaching { secs: 7 }));
                    let worker = app.link.as_ref().unwrap();
                    assert!(worker.send((None, ToWorker::Stop)).is_ok());
                    loop {
                        match worker.next_event() {
                            Ok(notice) => assert!(matches!(notice, (None, FromWorker::BlobWorker(_))), "{notice:?}"),
                            Err(TryRecvError::Empty) => std::thread::yield_now(),
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                    app.link_event(cx);
                }
            }
            assert!(app.link_queue.is_empty() && app.link.is_none());
            assert_eq!(app.view.connect.failed.as_deref(), Some(why));
            let join = app.ui.widget(cx, ids!(join_page));
            assert_eq!(join.label(cx, ids!(pair_status)).text(), shown);
            assert!(join.button(cx, ids!(pair_retry)).visible());
            assert!(app.view.connect.waiting_since.is_none() && app.model.as_ref().unwrap().pairing_since().is_none());
            assert_eq!(app.pair_timer.0, 0);
        }
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn draw_home_widget(cx: &mut Cx, widget: &WidgetRef, view: &mut makepad_create::lists::ViewData) {
        let pass = DrawPass::new(cx);
        let size = dvec2(650., 160.);
        pass.set_size(cx, size);
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent::default();
        let mut draw = cx_draw::CxDraw::new(cx, &event);
        let mut frame = Cx2d::new(&mut draw);
        frame.begin_pass(&pass, None);
        list.begin_always(&mut frame);
        frame.begin_root_turtle(size, Layout::flow_down());
        widget.draw_all(&mut frame, &mut Scope::with_data(view));
        frame.end_pass_sized_turtle();
        list.end(&mut frame);
        frame.end_pass(&pass);
    }

    fn drawn_member_row(cx: &Cx, members: &WidgetRef, member: MemberId) -> WidgetRef {
        let list = members.flat_list(cx, ids!(list));
        let list = list.borrow().unwrap();
        list.items.values().find(|item| {
            let chip = item.widget.widget(cx, ids!(select));
            matches!(chip.action_data().as_ref().and_then(|d| d.downcast_ref::<HomeAction>()),
                Some(HomeAction::Card { member: id, .. }) if *id == member)
        }).expect("visible member chip").widget.clone()
    }

    #[test]
    fn open_member_card_suppresses_its_chip_tooltip_on_focus_and_hover() {
        use makepad_town_core::{event::{Event as TownEvent, Member}, home::Home};
        use tip::{TipAction, TipWidgetRefExt};

        let ben = DeviceKey::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("town-member-tip-{}", makepad_town_core::hex(&ben.public())));
        let home = Home::open(&dir).unwrap();
        let (founder, _) = DeviceKey::load_or_create(&home.key_path()).unwrap();
        let (town, genesis) = TownEvent::found(&founder, 2, 0, 0, [0; 16]).unwrap();
        let mut log = Log::found(&home.town_log_path(&town), genesis).unwrap();
        let seat = append(&mut log, &founder, Content::Member(Member::Invite { subject: ben.public(), role: 0 }), None);
        append(&mut log, &ben, Content::Member(Member::Accept { entry: seat }), None);
        append(&mut log, &ben, Content::Profile { name: "Ben".into(), colour: 0 }, None);
        let ai = append(&mut log, &ben, Content::AgentAdded { name: "Claude Code".into(), kind: "session".into() }, None);
        drop(log);
        home.set_open_town(&town).unwrap();

        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, { use mod.widgets.* View{home_page := HomePage{visible: true}} });
            WidgetRef::script_from_value(vm, root)
        });
        app.model = Some(Model::open_home(home).unwrap().0);
        app.read_workspace();
        app.update_presence(cx);
        draw_home_widget(cx, &app.ui.widget(cx, ids!(home_page)), &mut app.view);
        let members = app.ui.widget(cx, ids!(home_members));
        let card = app.ui.widget(cx, ids!(member_card));
        let ben_row = drawn_member_row(cx, &members, MemberId::Seat(seat));
        let ai_row = drawn_member_row(cx, &members, MemberId::Agent(ai));
        let ben_chip = ben_row.button(cx, ids!(select));
        let ai_chip = ai_row.button(cx, ids!(select));
        let ben_tip = ben_row.tip(cx, ids!(tip));
        let ai_tip = ai_row.tip(cx, ids!(tip));
        let ben_status = ben_tip.borrow().unwrap().text.clone();
        let ai_status = "on Ben's computer";
        assert!(!ben_status.is_empty());
        assert_eq!(ai_tip.borrow().unwrap().text, ai_status);
        let deliver = |cx: &mut Cx, event: &Event| {
            cx.capture_actions(|cx| members.handle_event(cx, event, &mut Scope::empty()))
        };
        let raised = |actions: &Actions| actions.iter().filter_map(|a| a.as_widget_action()).any(|a|
            matches!(a.action.downcast_ref::<TipAction>(), Some(TipAction::HoverIn(..) | TipAction::HoverInWith(_))));
        let raised_with = |actions: &Actions, uid, text: &str| actions.iter().filter_map(|a| a.as_widget_action()).any(|a|
            a.widget_uid == uid && matches!(a.action.downcast_ref::<TipAction>(), Some(TipAction::HoverInWith(r)) if r.text == text));
        let focus = |prev, focus| Event::KeyFocus(KeyFocusEvent { prev, focus });
        let press = Event::A11yAction(a11y_tree::ActionRequest {
            req: 1, node: a11y_tree::NodeId::widget(ai_chip.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None,
        });
        let actions = deliver(cx, &press);
        app.connect_actions(cx, &actions);
        assert!(card.visible() && app.view.selected_member == Some(MemberId::Agent(ai)), "the press opens Ben's AI card");
        assert!(ai_tip.borrow().unwrap().text.is_empty(), "the selected tip clears before any redraw");
        cx.action(());
        cx.handle_actions();
        assert!(ai_chip.area().is_valid(cx) && cx.has_key_focus(ai_chip.area()), "the press gave the AI chip key focus");
        assert!(!raised(&deliver(cx, &focus(Area::Empty, ai_chip.area()))), "focus on the open card's chip raises no tip");
        let over = Event::MouseMove(MouseMoveEvent {
            abs: ai_chip.area().rect(cx).center(), lock_delta: Default::default(), window_id: WindowId(1, 1),
            modifiers: Default::default(), time: 0., handled: Default::default(),
        });
        assert!(!raised(&deliver(cx, &over)), "hover on the open card's chip raises no tip");
        assert_eq!(over.pointer_claimed_area(), ai_chip.area(), "the pointer reached the AI chip");
        let mut node = a11y_tree::Node::default();
        ai_chip.borrow().unwrap().semantics(cx, &mut node);
        assert_eq!(node.name, "Ben's AI · Claude Code, on Ben's computer");

        cx.set_key_focus(ben_chip.area());
        cx.action(());
        cx.handle_actions();
        assert!(cx.has_key_focus(ben_chip.area()));
        assert!(raised_with(&deliver(cx, &focus(ai_chip.area(), ben_chip.area())), ben_tip.widget_uid(), &ben_status),
            "Ben's chip still raises its own status while the AI card is open");
        app.handle_event(cx, &Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
        assert!(!card.visible() && app.view.selected_member.is_none(), "Escape closed the AI card");
        let actions = cx.capture_actions(|cx| draw_home_widget(cx, &members, &mut app.view));
        assert_eq!(ai_tip.borrow().unwrap().text, ai_status);
        assert!(!raised(&actions), "restoring the status on redraw does not raise a tip");
        cx.set_key_focus(ai_chip.area());
        cx.action(());
        cx.handle_actions();
        assert!(cx.has_key_focus(ai_chip.area()));
        assert!(raised_with(&deliver(cx, &focus(ben_chip.area(), ai_chip.area())), ai_tip.widget_uid(), ai_status),
            "returning focus to the AI chip raises its status again");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
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
        model.set_person_name("Ana").unwrap();
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
            app.update_presence(cx);
            draw_home_widget(cx, &content, &mut app.view);
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
        let member_row = |cx: &Cx, member| drawn_member_row(cx, &members, member);
        let member_chip = |cx: &Cx, member| member_row(cx, member).widget(cx, ids!(select));
        let chip = member_chip(cx, MemberId::You);
        home_action(&mut app, cx, chip);
        let badge = content.widget(cx, ids!(member_card.member_badge));
        assert!(badge.visible() && badge.widget(cx, ids!(you)).visible(), "your card shows your badge");
        assert_eq!(badge.label(cx, ids!(you.initial)).text(), "A", "your badge shows Ana's initial");
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
        assert!(badge.widget(cx, ids!(you)).visible());
        assert_eq!(badge.label(cx, ids!(you.initial)).text(), "K", "your reopened card shows Kit's initial");
        let escaped = cx.capture_actions(|cx| content.handle_event(cx, &escape, &mut Scope::empty()));
        app.connect_actions(cx, &escaped);
        cx.action(());
        cx.handle_actions();
        assert!(!card.visible() && !name.visible() && !cx.has_key_focus(name.area()),
            "Escape closes the card, hides the name field and releases its focus");
        let chip = member_chip(cx, MemberId::Agent(agent));
        home_action(&mut app, cx, chip);
        let ai_name = content.text_input(cx, ids!(ai_name_input));
        let ai_backend = content.label(cx, ids!(ai_backend));
        let member_name = content.label(cx, ids!(member_name));
        assert!(!member_name.visible() && ai_name.visible() && ai_name.text().is_empty(), "your AI starts with an empty name field");
        assert!(ai_backend.visible() && ai_backend.text() == "· Claude Code");
        let ai_status = &app.view.members.seats.iter().find(|s| s.id == MemberId::Agent(agent)).unwrap().status;
        assert_eq!(&content.label(cx, ids!(member_status)).text(), ai_status, "the AI detail uses its chip's status");
        assert!(content.widget(cx, ids!(member_more)).is_empty() && content.button(cx, ids!(remove_ai)).visible(),
            "the AI card offers Remove directly");

        draw_home(&mut app, cx);
        ai_name.take_key_focus(cx);
        cx.action(());
        cx.handle_actions();
        let enter = cx.capture_actions(|cx| {
            content.handle_event(cx, &Event::TextInput(TextInputEvent { input: "Scout".into(), ..Default::default() }), &mut Scope::empty());
            content.handle_event(cx, &Event::KeyDown(KeyEvent { key_code: KeyCode::ReturnKey, ..Default::default() }), &mut Scope::empty());
        });
        app.connect_actions(cx, &enter);
        assert!(!card.visible() && app.model.as_ref().unwrap().agent_custom_name(agent).as_deref() == Some("Scout"));
        draw_home(&mut app, cx);
        let row = member_row(cx, MemberId::Agent(agent));
        assert_eq!(row.label(cx, ids!(name)).text(), "Scout · Claude Code", "Return updates the existing chip");
        let chip = row.widget(cx, ids!(select));
        home_action(&mut app, cx, chip);
        draw_home(&mut app, cx);
        assert_eq!(ai_name.text(), "Scout", "the card opens with the saved name");
        ai_name.take_key_focus(cx);
        cx.action(());
        cx.handle_actions();
        assert!(!ai_name.area().is_empty() && cx.has_key_focus(ai_name.area()));
        ai_name.borrow_mut().unwrap().select_all(cx);
        content.handle_event(cx, &Event::TextInput(TextInputEvent { input: "Unsaved".into(), ..Default::default() }), &mut Scope::empty());
        assert_eq!(ai_name.text(), "Unsaved");
        let escaped = cx.capture_actions(|cx| app.handle_event(cx, &escape));
        assert!(ai_name.escaped(&escaped), "Escape reaches the focused name field");
        app.connect_actions(cx, &escaped);
        cx.action(());
        cx.handle_actions();
        assert!(!card.visible() && !ai_name.visible() && !cx.has_key_focus(ai_name.area()));
        assert_eq!(app.model.as_ref().unwrap().agent_custom_name(agent).as_deref(), Some("Scout"), "Escape discards the edit");

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
        app.ensure_worker().unwrap();
        for waiting in [false, true] {
            if waiting {
                let change = cx.capture_actions(|cx| input.replace_range(cx, 0..0, &peer_words, UndoGroup::New).unwrap());
                app.connect_actions(cx, &change);
                app.view.connect.apply(&FromWorker::Phase(Status::Reaching { secs: 0 }));
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
        app.link = None;
        // A computer this one linked with before, at an address the link remembered.
        let pool = DeviceKey::generate().unwrap().public();
        let model = app.model.as_mut().unwrap();
        model.pin_linked(&pool, 3, "Pool box").unwrap();
        let remembered_path = makepad_town_core::peers::PathMemory { addr: Some("192.0.2.2:4243".parse().unwrap()), lan: None };
        model.remember_path(&pool, remembered_path).unwrap();
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
        model.remember_path(&peer_key, remembered_path).unwrap();
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
        peer.set_person_name("Ben").unwrap();
        let code = Credential::parse(&peer.agent_code(None).unwrap()).unwrap();
        let (_, ben_ai) = peer.agent_join_with_id(code, true).unwrap();
        peer.act(Action::Provide { on: true }, Origin::Ui).unwrap();
        while let Some(bytes) = peer.pull_sync(model.me()) { model.receive_from(peer_key, &bytes).unwrap(); }
        let ShareState::Shared(shared) = model.share_state() else { panic!("both peers should have joined the offered town") };
        app.switch_town(cx, shared);
        draw_home(&mut app, cx);
        let chip = member_chip(cx, MemberId::Agent(ben_ai));
        home_action(&mut app, cx, chip);
        assert!(member_name.visible() && !ai_name.visible() && !ai_backend.visible(), "a peer AI has a read-only card");
        assert_eq!(member_name.text(), "Ben's AI · Claude Code");
        app.handle_event(cx, &escape);
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
        let peer_card = MemberId::Seat(app.model.as_ref().unwrap().state.seat_of(&peer_key).unwrap());
        let chip = member_chip(cx, peer_card);
        home_action(&mut app, cx, chip);
        assert_eq!(app.view.selected_member, Some(peer_card), "the peer chip owns the card");
        assert!(card.visible() && !pair_step.visible(), "the peer card temporarily hides the pairing form");
        app.handle_event(cx, &escape);
        assert!(!card.visible() && app.view.selected_member.is_none(), "Escape closes the peer card and clears its selection");
        assert_eq!(app.home_step, HomeStep::Pair(PairSide::Add(shared, AddComputer::Mine)));
        assert!(app.pair_step_visible() && pair_step.visible(), "Escape restores the pending pairing form");
        click(&mut app, cx, ids!(pair_close));
        let chip = member_chip(cx, peer_card);
        home_action(&mut app, cx, chip);
        assert_eq!(tip.borrow::<tip::Tip>().unwrap().text, "Add a member");
        assert!(!content.button(cx, ids!(hub_offer)).visible(), "another computer has no owner switch");
        let status = content.label(cx, ids!(member_status));
        let link = content.button(cx, ids!(device_link));
        let remove = content.button(cx, ids!(remove_member));
        let line = content.label(cx, ids!(removing_line));
        let undo = content.button(cx, ids!(undo_remove));
        let device_list = content.widget(cx, ids!(seat_devices));
        assert!(!device_list.visible(), "a single computer needs no device sub-lines");
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
        peer.remember_path(&guest, remembered_path).unwrap();
        peer.end_carrier(guest, true);
        std::mem::swap(app.model.as_mut().unwrap(), &mut peer);
        app.refresh(cx);
        draw_home(&mut app, cx);
        let guest_card = MemberId::Seat(app.model.as_ref().unwrap().state.seat_of(&guest).unwrap());
        let chip = member_chip(cx, guest_card);
        home_action(&mut app, cx, chip.clone());
        assert!(card.visible(), "the guest chip opens its card");
        assert_eq!(app.view.selected_member, Some(guest_card));
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
            let seat = app.view.members.seats.iter().find(|s| s.id == guest_card).unwrap();
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
        home_action(&mut app, cx, member_chip(cx, guest_card));

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
        use makepad_create::lists::Form;
        use makepad_town_core::event::{Chat, Event as TownEvent, Member};
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
        let mut add = |key: &DeviceKey, content: Content| append(&mut log, key, content, None);
        add(&ana, Content::Profile { name: "Ana".into(), colour: 0 });
        for (device, name) in [(&*ben, "Ben"), (&kit, "Kit")] {
            let invite = add(&ana, Content::Member(Member::Invite { subject: device.public(), role: 0 }));
            add(device, Content::Member(Member::Accept { entry: invite }));
            add(device, Content::Profile { name: name.into(), colour: 0 });
        }
        let ben_ai = add(&ben, Content::AgentAdded { name: "Claude Code".into(), kind: "session".into() });
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
        let second = MemberId::Seat(app.model.as_ref().unwrap().state.seat_of(&second_peer).unwrap());
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
        let ben_seat = MemberId::Seat(app.model.as_ref().unwrap().state.seat_of(&ben.public()).unwrap());
        let fill = |cx: &mut Cx, badge: WidgetRef| {
            let mut color = [f32::NAN; 4];
            badge.borrow::<View>().unwrap().draw_bg.get_instance(cx, id!(color), &mut color);
            color
        };
        let chip = member_row(cx, ben_seat).widget(cx, ids!(badge.peer));
        let ben_fill = fill(cx, chip);
        open_card(&mut app, cx, ben_seat);
        let peer_badge = badge.widget(cx, ids!(peer));
        assert!(badge.visible() && peer_badge.visible(), "Ben's card shows the peer badge");
        assert_eq!(fill(cx, peer_badge), ben_fill, "Ben's card and chip share a seat colour");
        open_card(&mut app, cx, MemberId::Agent(ben_ai));
        let ai = badge.widget(cx, ids!(peer_ai));
        assert!(ai.visible(), "Ben's AI card shows the peer AI badge");
        assert_eq!(fill(cx, ai), ben_fill, "Ben's AI wears Ben's seat colour");
        open_card(&mut app, cx, second);
        assert!(remove.visible() && remove.text() == "Remove", "the second peer's card offers Remove");
        start_removal(&mut app, cx);
        assert!(!badge.visible(), "pending removal hides the member badge");
        assert!(line.visible() && line.text() == format!("Removing {second_name} — that device can't rejoin this town.")
            && undo.visible() && !remove.visible(), "the pending removal names the selected peer and offers Undo");
        assert!(active(&app, first_peer) && active(&app, second_peer), "both peers remain during Undo");
        click(&mut app, cx, ids!(undo_remove));
        assert!(app.removing.is_none() && remove.visible() && !undo.visible(), "Undo restores the card actions");
        assert!(badge.visible() && badge.widget(cx, ids!(peer)).visible(), "Undo restores the peer badge");
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
        let you = &app.view.members.seats[0];
        assert_eq!((you.id, you.presence, you.status.as_str()), (MemberId::You, Presence::Away, "not in this town"));
        assert!(you.devices.is_empty());

        // Keep independent views of the fork for the founder, a sibling seat, and a reinstated device.
        let fork_founder_dir = dir.join("fork-founder");
        let fork_observer_dir = dir.join("fork-observer");
        let fork_target_dir = dir.join("fork-target");
        let fork_ana = Model::open(&fork_founder_dir).unwrap().0;
        let ana = fork_ana.device_key();
        let mira = Model::open(&fork_observer_dir).unwrap().0.device_key();
        let lu = Model::open(&fork_target_dir).unwrap().0.device_key();
        let rik = DeviceKey::generate().unwrap();
        let nora = DeviceKey::generate().unwrap();
        let fork_town = fork_ana.open_id();
        let fork_log_path = fork_ana.home.town_log_path(&fork_town);
        fork_ana.home.set_open_town(&fork_town).unwrap();
        drop(fork_ana);

        let mut log = Log::open(&fork_log_path).unwrap().0;
        let mut add = |key: &DeviceKey, content: Content| append(&mut log, key, content, None);
        let profile = |name: &str| Content::Profile { name: name.into(), colour: 0 };
        let invite = |device: &DeviceKey| Content::Member(Member::Invite { subject: device.public(), role: 0 });
        let accept = |entry| Content::Member(Member::Accept { entry });
        let chat = |text: &str| Content::Chat(Chat { text: text.into(), ..Chat::default() });
        add(&ana, profile("Ana"));
        let seat_rik = add(&ana, invite(&rik));
        add(&rik, accept(seat_rik));
        add(&rik, profile("Rik"));
        let seat_mira = add(&ana, invite(&mira));
        add(&mira, accept(seat_mira));
        add(&mira, profile("Mira"));
        let seat_lu = add(&rik, invite(&lu));
        add(&lu, accept(seat_lu));
        let lu_last = add(&lu, profile("Lu"));
        let vouch_nora = add(&rik, Content::Member(Member::AddDevice { seat: seat_rik, device: nora.public() }));
        add(&nora, accept(vouch_nora));
        let nora_last = add(&nora, profile("Nora"));
        let before_fork = add(&rik, chat("before fork"));
        let r_lu = add(&rik, Content::Member(Member::Remove { seat: seat_lu, device: lu.public(), last: lu_last }));
        let r_nora = add(&rik, Content::Member(Member::Remove { seat: seat_rik, device: nora.public(), last: nora_last }));
        let other_copy = append(&mut log, &rik, chat("other copy"), Some(before_fork));
        drop(log);
        // Rik's final exchange is already closed on its current boundary: the fork.
        makepad_town_core::home::Home::open(&fork_founder_dir).unwrap()
            .close_final(&fork_town, &rik.public(), makepad_town_core::fold::FinalClosure::fork(r_lu, other_copy).unwrap()).unwrap();
        for path in [&fork_observer_dir, &fork_target_dir] {
            let home = makepad_town_core::home::Home::open(path).unwrap();
            std::fs::copy(&fork_log_path, home.town_log_path(&fork_town)).unwrap();
            home.set_open_town(&fork_town).unwrap();
        }
        let mut fork_ana = Model::open(&fork_founder_dir).unwrap().0;
        let fork_mira = Model::open(&fork_observer_dir).unwrap().0;
        let fork_lu = Model::open(&fork_target_dir).unwrap().0;
        let reinstated: Vec<_> = fork_ana.reinstated_by_fork().into_iter()
            .map(|r| (r.forked_device, r.target_device, r.removal_event)).collect();
        assert_eq!(reinstated, [(rik.public(), lu.public(), r_lu), (rik.public(), nora.public(), r_nora)]);
        fork_ana.pin_linked(&rik.public(), VERSION, "Rik").unwrap();
        fork_ana.link_proved(&rik.public(), VERSION, "Rik", None).unwrap();
        let bare = "Rik's history split.";
        assert_eq!(fork_ana.chat_notice().expect("linked forked peer notice").line, bare);
        assert_eq!(fork_ana.home_notice().expect("linked forked peer on Home").line, bare);
        fork_ana.peer_version(Some(mira.public()), VERSION - 1);
        let prev_model = app.model.replace(fork_ana);
        app.refresh(cx);
        draw_home(&mut app, cx);

        let notice_share = content.button(cx, ids!(town_notice_share));
        let notice_ok = content.button(cx, ids!(town_notice_ok));
        let shown_notice = |text: &str, share: Option<&str>, ok: Option<&str>| {
            assert!(notice.visible(), "expected the Home notice for {text}");
            assert_eq!(notice_text.text(), text);
            assert_eq!(notice_share.visible().then(|| notice_share.text()).as_deref(), share);
            assert_eq!(notice_ok.visible().then(|| notice_ok.text()).as_deref(), ok);
        };
        let press_notice = |app: &mut App, cx: &mut Cx, path| {
            let actions = press(cx, content.widget(cx, path));
            app.handle_actions(cx, &actions);
        };
        let lu_line = "Rik's history split. Its removal of Lu no longer counts. (1 more)";
        let nora_line = "Rik's history split. Its removal of Nora no longer counts.";
        let lu_notice = || shown_notice(lu_line, Some("Remove Lu again"), Some("Keep Lu"));
        let nora_notice = || shown_notice(nora_line, Some("Remove Nora again"), Some("Keep Nora"));
        let bare_notice = || shown_notice(bare, Some("Start a new shared town with Rik"), None);
        let lu_card = MemberId::Seat(seat_lu);
        let nora_card = MemberId::Seat(seat_rik);
        shown_notice(&format!("{lu_line}\nMira's copy of Makepad Town is older; it will link again once it has updated."),
            Some("Remove Lu again"), Some("Keep Lu"));
        app.model.as_mut().unwrap().peer_version(Some(mira.public()), VERSION);
        app.update_town_notice(cx);
        lu_notice();
        open_card(&mut app, cx, lu_card);
        assert!(!remove.visible(), "Lu's card leaves removal to the notice");
        open_card(&mut app, cx, nora_card);
        assert!(remove.visible() && remove.text() == "Remove", "Nora's card still offers Remove");

        let stale_remove = press(cx, content.widget(cx, ids!(town_notice_share)));
        let stale_keep = press(cx, content.widget(cx, ids!(town_notice_ok)));
        for (page, form, pane) in [(Page::Project, Form::Wide, Pane::Page), (Page::Home, Form::Narrow, Pane::Chat)] {
            app.view.page = page;
            app.view.form = form;
            app.view.pane = pane;
            app.update_page(cx);
            assert!(!app.home_visible());
            app.handle_actions(cx, &stale_remove);
            app.handle_actions(cx, &stale_keep);
            assert!(app.removing.is_none() && app.dismissed_forks.is_empty(), "queued actions require visible Home");
        }
        app.view.form = Form::Wide;
        app.view.pane = Pane::Page;
        app.update_page(cx);
        press_notice(&mut app, cx, ids!(town_notice_share));
        assert!(app.removing.is_some() && line.visible() && undo.visible());
        assert_eq!(line.text(), "Removing Lu — that device can't rejoin this town.");
        assert_eq!(app.home_step, HomeStep::Card { town: fork_town, member: lu_card });
        assert!(matches!(app.removing.as_ref().map(|r| &r.target), Some(RemoveTarget::Device(k)) if *k == lu.public()));
        bare_notice();
        click(&mut app, cx, ids!(undo_remove));
        assert!(app.removing.is_none() && card.visible() && app.view.selected_member == Some(lu_card) && !remove.visible());
        lu_notice();

        open_card(&mut app, cx, nora_card);
        click(&mut app, cx, ids!(remove_member));
        let prompt = content.widget(cx, ids!(cascade_prompt));
        assert!(prompt.visible() && app.removing.is_none());
        lu_notice();
        click(&mut app, cx, ids!(cascade_only));
        assert!(app.removing.is_some());
        bare_notice();
        click(&mut app, cx, ids!(undo_remove));
        assert!(app.removing.is_none());
        lu_notice();

        open_card(&mut app, cx, lu_card);
        assert!(!remove.visible());
        press_notice(&mut app, cx, ids!(town_notice_ok));
        nora_notice();
        assert!(remove.visible() && remove.text() == "Remove", "Keep restores Remove on Lu's open card");
        open_card(&mut app, cx, nora_card);
        assert!(!remove.visible(), "Nora's removal now belongs to the notice");
        press_notice(&mut app, cx, ids!(town_notice_ok));
        bare_notice();
        assert!(remove.visible() && remove.text() == "Remove", "Keep restores Remove on Nora's open card");

        app.model.as_mut().unwrap().end_carrier(rik.public(), false);
        app.dismissed_forks.clear();
        app.update_town_notice(cx);
        app.update_member_card(cx);
        lu_notice();
        let expire_notice = |app: &mut App, cx: &mut Cx| {
            press_notice(app, cx, ids!(town_notice_share));
            let timer_id = app.removing.as_ref().expect("Remove again starts Undo").timer.0;
            assert!(undo.visible() && !notice.visible(), "only the card offers an action while Undo is pending");
            fire_timer(app, cx, timer_id);
            assert!(app.removing.is_none());
        };
        expire_notice(&mut app, cx);
        assert!(!active(&app, lu.public()) && active(&app, nora.public()), "expiry re-removes Lu alone");
        nora_notice();
        expire_notice(&mut app, cx);
        assert!(!active(&app, nora.public()) && !notice.visible(), "both removals now count");

        for viewer in [fork_mira, fork_lu] {
            app.model = Some(viewer);
            app.refresh(cx);
            draw_home(&mut app, cx);
            shown_notice(lu_line, None, Some("OK"));
        }
        open_card(&mut app, cx, MemberId::You);
        assert!(remove.visible() && remove.text() == "Leave", "reinstatement keeps Lu's own Leave action");

        let admit = |log: &mut Log, signer: &DeviceKey, peer: &DeviceKey, name: &str| {
            let seat = append(log, signer, invite(peer), None);
            append(log, peer, accept(seat), None);
            append(log, peer, profile(name), None);
            seat
        };
        let vouch = |log: &mut Log, seat, signer: &DeviceKey, peer: &DeviceKey, name: &str| {
            let entry = append(log, signer, Content::Member(Member::AddDevice { seat, device: peer.public() }), None);
            append(log, peer, accept(entry), None);
            append(log, peer, profile(name), None)
        };
        let row = |cx: &Cx, key| {
            let list = device_list.flat_list(cx, ids!(list));
            let list = list.borrow().unwrap();
            list.items[makepad_create::lists::device_item_id(&key)].widget.clone()
        };
        let remove_row = |app: &mut App, cx: &mut Cx, key| {
            let button = row(cx, key).widget(cx, ids!(device_remove));
            home_action(app, cx, button);
            app.removing.as_ref().expect("device Remove starts Undo").timer.0
        };

        // The founder and the invited person each have two computers.
        let multi_dir = dir.join("multi-ana1");
        let founder = Model::open(&multi_dir).unwrap().0;
        let multi_town = founder.open_id();
        let founder_seat = founder.state.seat_of(&founder.me()).unwrap();
        let ana1 = founder.device_key();
        let path = founder.home.town_log_path(&multi_town);
        founder.home.set_open_town(&multi_town).unwrap();
        drop(founder);
        let ana2 = DeviceKey::generate().unwrap();
        let ben1 = DeviceKey::generate().unwrap();
        let ben2 = DeviceKey::generate().unwrap();
        let mut log = Log::open(&path).unwrap().0;
        append(&mut log, &ana1, profile("Ana"), None);
        vouch(&mut log, founder_seat, &ana1, &ana2, "Ana2");
        let ben_seat = admit(&mut log, &ana1, &ben1, "Ben1");
        vouch(&mut log, ben_seat, &ben1, &ben2, "Ben2");
        drop(log);
        app.model = Some(Model::open(&multi_dir).unwrap().0);
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::You);
        draw_home(&mut app, cx);
        assert!(remove.visible() && remove.text() == "Leave" && device_list.visible());
        let local = row(cx, ana1.public());
        assert_eq!(local.label(cx, ids!(device_status)).text(), "this computer");
        assert!(!local.button(cx, ids!(device_connect)).visible() && !local.button(cx, ids!(device_remove)).visible());
        assert!(row(cx, ana2.public()).button(cx, ids!(device_remove)).visible());
        name.take_key_focus(cx);
        cx.action(());
        cx.handle_actions();
        assert!(!name.area().is_empty() && cx.has_key_focus(name.area()));
        remove_row(&mut app, cx, ana2.public());
        cx.action(());
        cx.handle_actions();
        assert_eq!(line.text(), "Removing Ana2 — that device can't rejoin this town.");
        assert!(undo.visible() && !device_list.visible() && !name.visible() && !cx.has_key_focus(name.area()));
        click(&mut app, cx, ids!(undo_remove));
        draw_home(&mut app, cx);
        assert!(active(&app, ana2.public()) && device_list.visible());
        let timer_id = remove_row(&mut app, cx, ana2.public());
        fire_timer(&mut app, cx, timer_id);
        assert!(!active(&app, ana2.public()) && active(&app, ana1.public()));
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::You);
        assert!(!device_list.visible() && !remove.visible(), "the sole founder computer cannot leave");

        open_card(&mut app, cx, MemberId::Seat(ben_seat));
        draw_home(&mut app, cx);
        assert!(device_list.visible() && remove.visible() && remove.text() == "Remove Ben1" && !link.visible());
        assert_eq!(app.view.member_devices.len(), 2);
        assert_eq!(row(cx, ben1.public()).label(cx, ids!(device_name)).text(), "Ben1");
        assert_eq!(row(cx, ben2.public()).label(cx, ids!(device_name)).text(), "Ben2");
        let timer_id = remove_row(&mut app, cx, ben2.public());
        assert_eq!(line.text(), "Removing Ben2 — that device can't rejoin this town.");
        fire_timer(&mut app, cx, timer_id);
        assert!(!active(&app, ben2.public()) && active(&app, ben1.public()));
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Seat(ben_seat));
        assert!(!device_list.visible() && remove.visible() && remove.text() == "Remove");
        assert_eq!(app.view.members.seats.iter().filter(|s| s.id == MemberId::Seat(ben_seat)).count(), 1);

        // Enter You directly from a fork notice for another computer of this person.
        let own_dir = dir.join("own-fork");
        let mut founder = Model::open(&own_dir).unwrap().0;
        founder.set_person_name("Ana").unwrap();
        let town = founder.open_id();
        let seat = founder.state.seat_of(&founder.me()).unwrap();
        let ana1 = founder.device_key();
        let path = founder.home.town_log_path(&town);
        founder.home.set_open_town(&town).unwrap();
        drop(founder);
        let ana2 = DeviceKey::generate().unwrap();
        let ana3 = DeviceKey::generate().unwrap();
        let mut log = Log::open(&path).unwrap().0;
        vouch(&mut log, seat, &ana1, &ana2, "Ana2");
        let last = vouch(&mut log, seat, &ana2, &ana3, "Ana3");
        let before = append(&mut log, &ana2, chat("before"), None);
        append(&mut log, &ana2, Content::Member(Member::Remove { seat, device: ana3.public(), last }), None);
        append(&mut log, &ana2, chat("other"), Some(before));
        drop(log);
        app.model = Some(Model::open(&own_dir).unwrap().0);
        app.refresh(cx);
        draw_home(&mut app, cx);
        name.set_text(cx, "");
        assert_eq!(app.home_step, HomeStep::Overview);
        assert!(notice_share.visible() && notice_share.text() == "Remove Ana3 again");
        press_notice(&mut app, cx, ids!(town_notice_share));
        assert_eq!(app.home_step, HomeStep::Card { town, member: MemberId::You });
        assert!(matches!(app.removing.as_ref().map(|r| &r.target), Some(RemoveTarget::Device(k)) if *k == ana3.public()));
        assert!(undo.visible() && !device_list.visible() && !name.visible());
        assert_eq!(line.text(), "Removing Ana3 — that device can't rejoin this town.");
        click(&mut app, cx, ids!(undo_remove));
        draw_home(&mut app, cx);
        assert!(app.removing.is_none() && card.visible() && app.view.selected_member == Some(MemberId::You));
        assert!(name.visible() && name.text() == "Ana", "opening You from the notice loads the computer name");
        assert!(device_list.visible() && notice_share.visible() && notice_share.text() == "Remove Ana3 again");
        assert!(!row(cx, ana3.public()).button(cx, ids!(device_remove)).visible());

        // Kit relays one batch that replaces the only computer in Ben's seat.
        let swap_dir = dir.join("swap-ana");
        let relay_dir = dir.join("swap-kit");
        let founder = Model::open(&swap_dir).unwrap().0;
        let relay = Model::open(&relay_dir).unwrap().0;
        let town = founder.open_id();
        let ana = founder.device_key();
        let kit = relay.device_key();
        let ben1 = DeviceKey::generate().unwrap();
        let ben2 = DeviceKey::generate().unwrap();
        let ana_log = founder.home.town_log_path(&town);
        let kit_log = relay.home.town_log_path(&town);
        founder.home.set_open_town(&town).unwrap();
        relay.home.set_open_town(&town).unwrap();
        drop((founder, relay));
        let mut log = Log::open(&ana_log).unwrap().0;
        append(&mut log, &ana, profile("Ana"), None);
        let seat_b = admit(&mut log, &ana, &ben1, "Ben1");
        admit(&mut log, &ana, &kit, "Kit");
        drop(log);
        std::fs::copy(&ana_log, &kit_log).unwrap();
        let mut founder = Model::open(&swap_dir).unwrap().0;
        let mut relay = Model::open(&relay_dir).unwrap().0;
        for (device, label) in [(&ben1, "Ben1"), (&ben2, "Ben2")] {
            founder.pin_linked(&device.public(), VERSION, label).unwrap();
            founder.remember_path(&device.public(), remembered_path).unwrap();
        }
        founder.pin_linked(&kit.public(), VERSION, "Kit").unwrap();
        relay.pin_linked(&ana.public(), VERSION, "Ana").unwrap();
        founder.link_proved(&kit.public(), VERSION, "Kit", None).unwrap();
        relay.link_proved(&ana.public(), VERSION, "Ana", None).unwrap();
        loop {
            let outgoing = founder.pull_sync(kit.public());
            let incoming = relay.pull_sync(ana.public());
            if outgoing.is_none() && incoming.is_none() { break }
            if let Some(bytes) = outgoing { relay.receive_from(ana.public(), &bytes).unwrap(); }
            if let Some(bytes) = incoming { founder.receive_from(kit.public(), &bytes).unwrap(); }
        }
        app.model = Some(founder);
        app.carriers = Carriers::default();
        app.view.connect = ConnectView::default();
        app.link_queue.clear();
        let kit_at = Attempt { carrier: Carrier::Peer(kit.public()), generation: 1 };
        app.carriers.up.insert(kit_at, "Kit".into());
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Seat(seat_b));
        let header = content.label(cx, ids!(member_name));
        assert_eq!(header.text(), "Ben1");
        assert!(!device_list.visible() && remove.visible() && link.visible());
        let stale_remove = press(cx, content.widget(cx, ids!(remove_member)));
        let stale_connect = press(cx, content.widget(cx, ids!(device_link)));

        drop(relay);
        let start = std::fs::metadata(&kit_log).unwrap().len() as usize;
        let mut log = Log::open(&kit_log).unwrap().0;
        let entry = append(&mut log, &ben1, Content::Member(Member::AddDevice { seat: seat_b, device: ben2.public() }), None);
        append(&mut log, &ben2, accept(entry), None);
        append(&mut log, &ben2, profile("Ben2"), None);
        append(&mut log, &ben1, Content::Member(Member::Remove { seat: seat_b, device: ben1.public(), last: entry }), None);
        drop(log);
        let bytes = std::fs::read(&kit_log).unwrap();
        let batch = Message::Events { town, records: &bytes[start..] }.encode();
        let (refresh, ..) = pump(&mut app, cx, Some(kit_at), [FromWorker::Message(batch)]);
        assert!(refresh && !active(&app, ben1.public()) && active(&app, ben2.public()));
        assert_eq!(app.home_step, HomeStep::Card { town, member: MemberId::Seat(seat_b) });
        assert_eq!(header.text(), "Ben2");
        assert!(remove.visible() && link.visible(), "fresh clicks are eligible for the replacement computer");
        let queued = app.link_queue.len();
        app.connect_actions(cx, &stale_remove);
        assert!(app.removing.is_none() && active(&app, ben2.public()), "a queued Remove for Ben1 cannot remove Ben2");
        app.connect_actions(cx, &stale_connect);
        assert!(app.view.connect.peer.is_none() && app.link_queue.len() == queued && !app.carriers.wants_device(&ben2.public()),
            "a queued Connect for Ben1 cannot reach Ben2");
        start_removal(&mut app, cx);
        assert_eq!(line.text(), "Removing Ben2 — that device can't rejoin this town.");
        click(&mut app, cx, ids!(undo_remove));
        click(&mut app, cx, ids!(device_link));
        assert_eq!(app.view.connect.peer, Some(ben2.public()));
        app.connect_stop(cx);

        // Ben's two computers share one seat. Kit joined through Ben, and Lu can join through Kit.
        let family = |home: &std::path::Path, kit: &DeviceKey, with_lu: bool| {
            let founder = Model::open(home).unwrap().0;
            let town = founder.open_id();
            let ana = founder.device_key();
            let path = founder.home.town_log_path(&town);
            founder.home.set_open_town(&town).unwrap();
            drop(founder);
            let mut log = Log::open(&path).unwrap().0;
            append(&mut log, &ana, profile("Ana"), None);
            let seat_b = admit(&mut log, &ana, &ben1, "Ben");
            vouch(&mut log, seat_b, &ben1, &ben2, "Ben2");
            admit(&mut log, &ben1, kit, "Kit");
            let seat_l = with_lu.then(|| admit(&mut log, kit, &lu, "Lu"));
            drop(log);
            (Model::open(home).unwrap().0, seat_b, seat_l)
        };
        let head = card.widget(cx, ids!(member_head));
        let question = prompt.label(cx, ids!(cascade_question));
        let all = prompt.button(cx, ids!(cascade_all));
        let only = prompt.button(cx, ids!(cascade_only));
        let expire = |app: &mut App, cx: &mut Cx| {
            let timer_id = app.removing.as_ref().expect("seat removal must offer Undo before expiry").timer.0;
            fire_timer(app, cx, timer_id);
        };
        let everyone_active = |app: &App, keys: &[PublicKey]| keys.iter().all(|&key| active(app, key));
        let (founder, seat_b, seat_l) = family(&dir.join("cascade-ana"), &kit, true);
        let founder_key = founder.me();
        app.model = Some(founder);
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Seat(seat_l.unwrap()));
        click(&mut app, cx, ids!(remove_member));
        assert!(!prompt.visible() && app.confirm_cascade.is_none() && app.removing.is_some());
        assert_eq!(line.text(), "Removing Lu — that device can't rejoin this town.");
        click(&mut app, cx, ids!(undo_remove));

        open_card(&mut app, cx, MemberId::Seat(seat_b));
        draw_home(&mut app, cx);
        assert!(remove.visible() && remove.text() == "Remove Ben" && device_list.visible());
        click(&mut app, cx, ids!(remove_member));
        assert!(prompt.visible() && !head.visible() && !device_list.visible() && app.removing.is_none());
        assert_eq!(question.text(), "Also remove Kit and Lu, who joined through Ben?");
        assert_eq!(all.text(), "Remove all 3");
        assert_eq!(only.text(), "Only Ben");
        app.handle_event(cx, &escape);
        assert!(!prompt.visible() && card.visible() && head.visible() && device_list.visible() && app.confirm_cascade.is_none());
        app.handle_event(cx, &escape);
        assert!(!card.visible() && app.view.selected_member.is_none());
        open_card(&mut app, cx, MemberId::Seat(seat_b));
        click(&mut app, cx, ids!(remove_member));
        click(&mut app, cx, ids!(cascade_cancel));
        assert!(!prompt.visible() && card.visible() && head.visible() && device_list.visible());

        let family_keys = [ben1.public(), ben2.public(), kit.public(), lu.public()];
        for (choice, expected) in [
            (ids!(cascade_only), "Removing Ben (2 computers) — those devices can't rejoin this town."),
            (ids!(cascade_all), "Removing Ben, Kit and Lu — those devices can't rejoin this town."),
        ] {
            click(&mut app, cx, ids!(remove_member));
            click(&mut app, cx, choice);
            assert!(undo.visible() && !prompt.visible() && !device_list.visible());
            assert_eq!(line.text(), expected);
            click(&mut app, cx, ids!(undo_remove));
            assert!(everyone_active(&app, &family_keys) && device_list.visible(), "Undo keeps the entire family");
        }
        click(&mut app, cx, ids!(remove_member));
        click(&mut app, cx, ids!(cascade_all));
        expire(&mut app, cx);
        assert!(family_keys.iter().all(|&key| !active(&app, key)) && active(&app, founder_key));
        assert!(app.view.share_error.is_none() && !card.visible());

        let (founder, seat_b, _) = family(&dir.join("cascade-only-ana"), &kit, false);
        let founder_key = founder.me();
        app.model = Some(founder);
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Seat(seat_b));
        click(&mut app, cx, ids!(remove_member));
        assert_eq!(question.text(), "Also remove Kit, who joined through Ben?");
        assert_eq!(all.text(), "Remove both");
        click(&mut app, cx, ids!(cascade_only));
        expire(&mut app, cx);
        assert!(!active(&app, ben1.public()) && !active(&app, ben2.public()) && active(&app, kit.public()) && active(&app, founder_key));
        assert!(app.view.share_error.is_none());

        // Deliver new grants from Kit through the same link and refresh path as incoming sync.
        let relay_dir = dir.join("cascade-live-kit");
        let relay = Model::open(&relay_dir).unwrap().0;
        let kit = relay.device_key();
        let (mut founder, seat_b, _) = family(&dir.join("cascade-live-ana"), &kit, false);
        let town = founder.open_id();
        let ana = founder.me();
        let kit_log = relay.home.town_log_path(&town);
        relay.home.set_open_town(&town).unwrap();
        drop(relay);
        std::fs::copy(founder.home.town_log_path(&town), &kit_log).unwrap();
        let mut relay = Model::open(&relay_dir).unwrap().0;
        founder.pin_linked(&kit.public(), VERSION, "Kit").unwrap();
        relay.pin_linked(&ana, VERSION, "Ana").unwrap();
        founder.link_proved(&kit.public(), VERSION, "Kit", None).unwrap();
        relay.link_proved(&ana, VERSION, "Ana", None).unwrap();
        loop {
            let outgoing = founder.pull_sync(kit.public());
            let incoming = relay.pull_sync(ana);
            if outgoing.is_none() && incoming.is_none() { break }
            if let Some(bytes) = outgoing { relay.receive_from(ana, &bytes).unwrap(); }
            if let Some(bytes) = incoming { founder.receive_from(kit.public(), &bytes).unwrap(); }
        }
        app.model = Some(founder);
        app.carriers = Carriers::default();
        app.view.connect = ConnectView::default();
        app.link_queue.clear();
        let kit_at = Attempt { carrier: Carrier::Peer(kit.public()), generation: 1 };
        app.carriers.up.insert(kit_at, "Kit".into());
        drop(relay);
        let deliver = |app: &mut App, cx: &mut Cx, change: &dyn Fn(&mut Log)| {
            let start = std::fs::metadata(&kit_log).unwrap().len() as usize;
            let mut log = Log::open(&kit_log).unwrap().0;
            change(&mut log);
            drop(log);
            let bytes = std::fs::read(&kit_log).unwrap();
            let batch = Message::Events { town, records: &bytes[start..] }.encode();
            let (refresh, ..) = pump(app, cx, Some(kit_at), [FromWorker::Message(batch)]);
            assert!(refresh, "a delivered membership batch refreshes the card");
        };
        app.refresh(cx);
        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Seat(seat_b));
        click(&mut app, cx, ids!(remove_member));
        assert_eq!(question.text(), "Also remove Kit, who joined through Ben?");
        assert_eq!(all.text(), "Remove both");
        let stale_all = press(cx, content.widget(cx, ids!(cascade_all)));
        deliver(&mut app, cx, &|log| { admit(log, &kit, &mira, "Mira"); });
        click(&mut app, cx, ids!(cascade_all));
        assert!(prompt.visible() && app.removing.is_none());
        assert_eq!(question.text(), "Also remove Kit and Mira, who joined through Ben?");
        assert_eq!(all.text(), "Remove all 3");
        app.connect_actions(cx, &stale_all);
        assert!(prompt.visible() && app.removing.is_none(), "a click on the earlier question cannot confirm the new names");
        click(&mut app, cx, ids!(cascade_all));
        assert!(app.removing.is_some());
        deliver(&mut app, cx, &|log| { admit(log, &kit, &lu, "Lu"); });
        expire(&mut app, cx);
        assert_eq!(app.view.share_error.as_deref(), Some("The town changed before the removal could finish."));
        assert!(everyone_active(&app, &[ben1.public(), ben2.public(), kit.public(), mira.public(), lu.public()]));

        draw_home(&mut app, cx);
        open_card(&mut app, cx, MemberId::Seat(seat_b));
        let stale_seat_remove = press(cx, content.widget(cx, ids!(remove_member)));
        let ben3 = DeviceKey::generate().unwrap();
        deliver(&mut app, cx, &|log| { vouch(log, seat_b, &ben1, &ben3, "Ben3"); });
        app.connect_actions(cx, &stale_seat_remove);
        assert!(app.confirm_cascade.is_none() && app.removing.is_none(), "the earlier card did not show the third computer");
        app.connect_stop(cx);
        app.model = prev_model;

        std::mem::swap(app.model.as_mut().unwrap(), &mut joiner);
        app.refresh(cx);
        draw_home(&mut app, cx);
        let first = MemberId::Seat(app.model.as_ref().unwrap().state.seat_of(&first_peer).unwrap());
        open_card(&mut app, cx, first);
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
        assert_eq!(app.call.open_town(), Some((town_a, &[ben][..])), "dial Ben in the open town");
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
        assert_eq!(app.call.open_town(), Some((town_b, &[kit][..])), "an idle switch follows the open town");
        assert_eq!(app.call.linked_peers().count(), 2);
        assert!(app.link_queue.is_empty());

        ports[&kit1].messages_in.try_send(Message::Ring {
            call_id: [3; 16], secret: [4; 32], video: false, ext: makepad_create::call::RingExt { town: Some(town_b), ..Default::default() },
        }.encode()).unwrap();
        app.call.poll(cx);
        assert_eq!(app.call.view().phase, Phase::Ringing);
        app.model.as_mut().unwrap().open_town(town_a).unwrap();
        app.sync_call_carriers(cx);
        assert_eq!((app.call.ringing_from(), app.call.ringing_call_town()), (Some(kit), Some(town_b)), "the ringing call stays with Kit");
        assert_eq!(app.call.linked_peers().collect::<BTreeSet<_>>(), BTreeSet::from([ben, kit]));
        assert!(app.link_queue.is_empty());
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn switch_town_updates_open_town_call_targets() {
        use makepad_create::call::{CallEnv, Message, Phase, RingExt};
        use makepad_town_core::link::VERSION;
        use std::sync::Arc;

        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        let dir = std::env::temp_dir().join(format!("town-switch-call-{}", makepad_town_core::hex(&DeviceKey::generate().unwrap().public())));
        let mut model = Model::open(&dir.join("Alice")).unwrap().0;
        let solo = model.open_id();
        let [(ben, town_a), (kit, town_b)] = ["Ben", "Kit"].map(|name| {
            let mut peer = Model::open(&dir.join(name)).unwrap().0;
            let (me, key) = (model.me(), peer.me());
            model.pin_linked(&key, VERSION, name).unwrap();
            peer.pin_linked(&me, VERSION, "Alice").unwrap();
            model.link_proved(&key, VERSION, name, None).unwrap();
            peer.link_proved(&me, VERSION, "Alice", None).unwrap();
            let exchange = |model: &mut Model, peer: &mut Model| loop {
                let (a, b) = (model.pull_sync(key), peer.pull_sync(me));
                if a.is_none() && b.is_none() { break }
                if let Some(frame) = a { peer.receive_from(me, &frame).unwrap(); }
                if let Some(frame) = b { model.receive_from(key, &frame).unwrap(); }
            };
            exchange(&mut model, &mut peer);
            peer.offer_town(true).unwrap();
            exchange(&mut model, &mut peer);
            let prompt = model.offer_prompt().unwrap();
            assert!(model.answer_prompt(prompt, true), "the shared town offer must be accepted");
            exchange(&mut model, &mut peer);
            app.carriers.up.insert(Attempt { carrier: Carrier::Peer(key), generation: 1 }, name.into());
            (key, prompt.town)
        });
        app.model = Some(model);
        app.call.configure(CallEnv { files: Default::default(), devices_path: dir.join("devices"),
            signal: Arc::new(|| {}), voice: makepad_call::driver::VoiceSource::Off,
            spawn: Arc::new(|_, _| panic!("ringing must not start media")), jobs: Arc::new(|_, _| panic!("ringing needs no audio job")) });
        app.switch_town(cx, town_a);
        assert_eq!(app.call.open_town(), Some((town_a, &[ben][..])), "switching directly selects Ben");
        let ports: BTreeMap<_, _> = app.link_queue.drain(..).map(|cmd| match cmd {
            (Some(Attempt { carrier: Carrier::Peer(peer), .. }), ToWorker::Call(port)) => (peer, port),
            _ => panic!("expected a call carrier attachment"),
        }).collect();
        app.switch_town(cx, town_b);
        assert_eq!(app.call.open_town(), Some((town_b, &[kit][..])), "Kit becomes dialable before the next carrier poll");
        assert!(app.view.call.can_call && app.view.call.view.peer_name == "Kit", "the Call button immediately names Kit");
        assert!(app.view.call.can_share, "Kit can receive a share in this town");
        app.switch_town(cx, solo);
        assert_eq!(app.call.open_town(), Some((solo, &[][..])), "the solo town has no call targets");
        assert!(!app.view.call.can_call && app.view.call.view.peer_name.is_empty(), "the solo town disables calling and clears the name");
        assert!(!app.view.call.can_share, "a solo town has no share recipients");
        assert!(app.view.call.linked, "one-to-one sharing still has its carriers");
        let ring = Message::Ring { call_id: [4; 16], video: false, secret: [0; 32],
            ext: RingExt { town: Some(town_b), ..Default::default() } }.encode();
        ports[&ben].messages_in.try_send(ring.clone()).unwrap();
        app.call.poll(cx);
        assert_eq!(app.call.view().phase, Phase::Idle, "Ben cannot ring in Kit's town");
        ports[&kit].messages_in.try_send(ring).unwrap();
        app.call.poll(cx);
        assert_eq!((app.call.ringing_from(), app.call.ringing_call_town()), (Some(kit), Some(town_b)),
            "Kit's background-town ring reaches the solo town's open window");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    pub(crate) fn attach_call_seat(cx: &mut Cx, model: Model, peers: &[(PublicKey, &str)]) -> (App, BTreeMap<PublicKey, makepad_create::link_worker::MediaPort>) {
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_create::call::CallEnv;
        use std::sync::Arc;
        let mut app = cx.with_vm(App::script_new);
        app.call.configure(CallEnv { files: model.home.files(), devices_path: model.home.call_devices_path(),
            voice: VoiceSource::Off, spawn: std_spawn(), jobs: std_spawn(), signal: Arc::new(|| {}) });
        app.model = Some(model);
        for &(peer, name) in peers {
            app.carriers.up.insert(Attempt { carrier: Carrier::Peer(peer), generation: 1 }, name.into());
        }
        app.sync_call_carriers(cx);
        app.show_call(cx);
        let ports: BTreeMap<_, _> = app.link_queue.drain(..).map(|cmd| match cmd {
            (Some(Attempt { carrier: Carrier::Peer(peer), .. }), ToWorker::Call(port)) => (peer, port),
            _ => panic!("each compatible carrier gets its call port"),
        }).collect();
        (app, ports)
    }

    #[test]
    fn step3_real_hello_completes_town_call_and_share_and_excludes_legacy_peer() {
        use makepad_create::{call::{Fact, Message, Phase, Press, ShareEndReason, ShareOfferExt}, link_worker::MediaPort};
        use makepad_town_core::{event::{Actor, Content, Draft, Event as TownEvent, Member, Via}, home::Home,
            link::VERSION, log::Log, share::{hello::{Hello, MediaCaps}, Message as SyncMessage}};

        fn exchange(a: &mut Model, b: &mut Model) {
            loop {
                let (out, back) = (a.pull_sync(b.me()), b.pull_sync(a.me()));
                if out.is_none() && back.is_none() { break }
                if let Some(frame) = out { b.receive_from(a.me(), &frame).unwrap(); }
                if let Some(frame) = back { a.receive_from(b.me(), &frame).unwrap(); }
            }
        }

        fn pump(cx: &mut Cx, a: &mut App, ab: &MediaPort, b: &mut App, ba: &MediaPort) {
            loop {
                a.call.poll(cx);
                b.call.poll(cx);
                let mut sent = false;
                for (from, to) in [(ab, ba), (ba, ab)] {
                    for frame in from.messages_out.try_iter() {
                        to.messages_in.try_send(frame).expect("forward the linked peer's signaling");
                        sent = true;
                    }
                }
                if !sent { break }
            }
        }

        let old = DeviceKey::generate().unwrap();
        let legacy = old.public();
        let dir = std::env::temp_dir().join(format!("town-call-greeting-{}", makepad_town_core::hex(&legacy)));
        let home = Home::open(&dir.join("Ana")).unwrap();
        let (founder, _) = DeviceKey::load_or_create(&home.key_path()).unwrap();
        let (town, genesis) = TownEvent::found(&founder, 2, 0, 0, [0; 16]).unwrap();
        // Give the legacy carrier genuine membership before either current copy connects.
        {
            let mut log = Log::found(&home.town_log_path(&town), genesis).unwrap();
            let mut sign = |key: &DeviceKey, member| {
                let content = Content::Member(member);
                let event = TownEvent::create(key, town, Draft { kind: content.kind(), kind_version: content.version(),
                    critical: true, actor: Actor::Human, via: Via::Ui, wall_ms: 0,
                    parents: log.parents_for(&key.public()), content: &content.encode() }, [0; 16]).unwrap();
                let id = event.id;
                log.append(event).unwrap();
                id
            };
            let entry = sign(&founder, Member::Invite { subject: legacy, role: 0 });
            sign(&old, Member::Accept { entry });
        }
        home.set_open_town(&town).unwrap();
        let mut a = Model::open_home(home).unwrap().0;
        let mut b = Model::open(&dir.join("Ben")).unwrap().0;
        let (ak, bk) = (a.me(), b.me());
        for (model, peer, name) in [(&mut a, bk, "Ben"), (&mut b, ak, "Ana")] {
            model.pin_linked(&peer, VERSION, name).unwrap();
            model.link_proved(&peer, VERSION, name, None).unwrap();
        }
        exchange(&mut a, &mut b);
        a.offer_town(false).unwrap();
        exchange(&mut a, &mut b);
        let prompt = b.offer_prompt().expect("Ben receives Ana's town offer");
        assert!(b.answer_prompt(prompt, true), "Ben accepts Ana's town");
        exchange(&mut a, &mut b);
        a.pin_linked(&legacy, VERSION, "Old").unwrap();
        a.link_proved(&legacy, VERSION, "Old", None).unwrap();
        a.receive_from(legacy, &SyncMessage::Hello(Hello {
            level: 3, min_peer: 0, sources: Vec::new(), media_caps: MediaCaps::NONE,
        }).encode()).unwrap();
        assert_eq!(a.verify_wire_town(town, Some(legacy)), Some(town), "the legacy peer is a linked member of the call town");

        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let (mut ana, a_ports) = attach_call_seat(cx, a, &[(bk, "Ben"), (legacy, "Old")]);
        let (mut ben, b_ports) = attach_call_seat(cx, b, &[(ak, "Ana")]);
        let (ab, ba, old_port) = (&a_ports[&bk], &b_ports[&ak], &a_ports[&legacy]);
        assert_eq!(ana.call.linked_peers().collect::<BTreeSet<_>>(), BTreeSet::from([bk, legacy]), "both carriers stay attached");
        assert_eq!(ana.call.open_town(), Some((town, &[bk][..])), "only Ben's real greeting makes a member dialable");
        old_port.messages_in.try_send(Message::Ring {
            call_id: [1; 16], secret: [0; 32], video: false, ext: Default::default(),
        }.encode()).unwrap();
        pump(cx, &mut ana, ab, &mut ben, ba);
        assert_eq!(ana.call.view().phase, Phase::Idle, "a townless ring cannot open an incoming call");

        ana.call.press(cx, Press::Call);
        pump(cx, &mut ana, ab, &mut ben, ba);
        let session = ben.call.ringing_session().expect("Ana's call rings Ben");
        assert_eq!(ana.call.take_facts(), [Fact::Open { session, video: false, town, target: makepad_create::call::CallTarget::Town }], "Ana opens the shared town's call");
        ben.call.press(cx, Press::Answer);
        pump(cx, &mut ana, ab, &mut ben, ba);
        assert_eq!(ben.call.take_facts(), [Fact::Join { session, from: ak, town }], "Ben joins Ana in the same town");
        assert_eq!((ana.call.view().phase, ben.call.view().phase), (Phase::Live, Phase::Live), "both ends are live after Answer");
        assert!(ana.call.has_call_workers() && ben.call.has_call_workers(), "the linked ports negotiate media in both directions");
        ana.call.press(cx, Press::HangUp);
        pump(cx, &mut ana, ab, &mut ben, ba);
        assert_eq!((ana.call.view().phase, ben.call.view().phase), (Phase::Idle, Phase::Idle), "HangUp reaches both ends");

        let ben_share = [2; 16];
        let offer = |session| Message::Share { session, title: "Drawing".into(), w: 640., h: 480., ext: ShareOfferExt { town: Some(town), ..Default::default() } }.encode();
        ab.messages_in.try_send(offer(ben_share)).unwrap();
        pump(cx, &mut ana, ab, &mut ben, ba);
        assert!(ana.call.shown().share.offer(ben_share).is_some(), "Ben's offer arrives over the one-to-one share link");
        old_port.messages_in.try_send(offer([3; 16])).unwrap();
        ana.call.poll(cx);
        let sessions: Vec<_> = ana.call.shown().share.offers.iter().map(|offer| offer.session).collect();
        assert_eq!(sessions, [ben_share], "another carrier cannot add or replace an offer while Ben is sharing");
        ab.messages_in.try_send(Message::ShareOff { session: ben_share, reason: ShareEndReason::Stopped }.encode()).unwrap();
        pump(cx, &mut ana, ab, &mut ben, ba);
        assert!(ana.call.shown().share.offers.is_empty(), "Ben's ShareOff closes the offer");

        ana.call.shutdown(cx);
        ben.call.shutdown(cx);
        while ana.call.has_call_workers() || ben.call.has_call_workers() {
            ana.call.poll(cx);
            ben.call.poll(cx);
            std::thread::yield_now();
        }
        drop((ana, ben));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn step4_real_hello_shares_with_step4_peer_calls_step3_peer_and_gates_step3_share() {
        use makepad_create::{call::{CallAction, Fact, Message, Phase, Press, ShareChoice, ShareEndReason, ShareOfferExt, ShareRole},
            link_worker::MediaPort};
        use makepad_town_core::{link::VERSION, share::{self, hello::{Hello, MediaCaps, LEVEL, MULTIPARTY_CALL_LEVEL,
            MULTIPARTY_SHARE_LEVEL}, Message as SyncMessage}};

        fn link(a: &mut Model, b: &mut Model, name: &str) {
            let (ak, bk) = (a.me(), b.me());
            for (model, key, label) in [(a, bk, name), (b, ak, "Ana")] {
                model.pin_linked(&key, VERSION, label).unwrap();
                model.link_proved(&key, VERSION, label, None).unwrap();
            }
        }

        fn exchange(a: &mut Model, b: &mut Model, greeting_from_b: Option<&Hello>) {
            loop {
                let (out, back) = (a.pull_sync(b.me()), b.pull_sync(a.me()));
                if out.is_none() && back.is_none() { break }
                if let Some(frame) = out { b.receive_from(a.me(), &frame).unwrap(); }
                if let Some(frame) = back {
                    let bytes = match greeting_from_b {
                        Some(hello) if share::is_hello(&frame) => SyncMessage::Hello(hello.clone()).encode(),
                        _ => frame,
                    };
                    a.receive_from(b.me(), &bytes).unwrap();
                }
            }
        }

        fn invite(a: &mut Model, b: &mut Model, greeting_from_b: Option<&Hello>) {
            a.offer_town(false).expect("Ana can offer her open town to the new carrier");
            exchange(a, b, greeting_from_b);
            let prompt = b.offer_prompt().expect("the invitee receives the town offer");
            assert_eq!(prompt.town, a.open_id(), "the prompt identifies Ana's town");
            assert!(b.answer_prompt(prompt, true), "the invitee accepts Ana's town");
            exchange(a, b, greeting_from_b);
        }

        fn pump(cx: &mut Cx, apps: &mut [&mut App], links: &[(&MediaPort, &MediaPort)]) {
            loop {
                for app in apps.iter_mut() { app.call.poll(cx); }
                let mut sent = false;
                for &(a, b) in links {
                    for (from, to) in [(a, b), (b, a)] {
                        for frame in from.messages_out.try_iter() {
                            to.messages_in.try_send(frame).expect("the receiving port accepts the call signal");
                            sent = true;
                        }
                    }
                }
                if !sent { break }
            }
        }

        let dir = std::env::temp_dir().join(format!("town-share-greeting-{}", makepad_town_core::hex(&DeviceKey::generate().unwrap().public())));
        let [mut a, mut b, mut k] = ["Ana", "Ben", "Kit"].map(|seat| Model::open(&dir.join(seat)).unwrap().0);
        let (town, town_s3) = (a.open_id(), k.open_id());
        let (ak, bk, kk) = (a.me(), b.me(), k.me());
        let kit_hello = Hello { level: MULTIPARTY_CALL_LEVEL, min_peer: 0, sources: Vec::new(),
            media_caps: MediaCaps(MediaCaps::MULTIPARTY_V1) };

        link(&mut a, &mut b, "Ben");
        exchange(&mut a, &mut b, None);
        invite(&mut a, &mut b, None);
        a.end_carrier(bk, false);
        b.end_carrier(ak, false);

        link(&mut a, &mut k, "Kit");
        let first = a.pull_sync(kk).expect("Ana greets Kit before sending any sync data");
        let Some(SyncMessage::Hello(ana_hello)) = SyncMessage::decode(&first).unwrap() else { panic!("the first sync frame must be Hello") };
        assert!(kit_hello.compatible(&ana_hello) && ana_hello.compatible(&kit_hello), "both levels permit town sync");
        assert_eq!(ana_hello.level, LEVEL, "the wire greeting uses the local level");
        assert!(ana_hello.level >= MULTIPARTY_SHARE_LEVEL && MULTIPARTY_SHARE_LEVEL > MULTIPARTY_CALL_LEVEL,
            "town shares require a newer level than town calls");
        assert!(ana_hello.level > kit_hello.level && kit_hello.needs_update(&ana_hello) && !ana_hello.needs_update(&kit_hello),
            "only the voice-only copy needs an update");
        assert!(ana_hello.media_caps.supports(MediaCaps::MULTIPARTY_V1) && ana_hello.media_caps.supports(MediaCaps::MULTIPARTY_SHARE_V1),
            "Ana advertises town calls and town shares");
        k.receive_from(ak, &first).unwrap();
        exchange(&mut a, &mut k, Some(&kit_hello));
        invite(&mut a, &mut k, Some(&kit_hello));

        let own = Invite { nat: Hint::Easy, addr: "192.0.2.1:4100".parse().unwrap(), lan: None, commit: 1 };
        let peer = Invite { addr: "192.0.2.2:4200".parse().unwrap(), ..own.clone() };
        let k_words = PairWords { own: own.clone(), peer: peer.clone() };
        let a_words = PairWords { own: peer, peer: own };
        k.pair(PairSide::Add(town_s3, AddComputer::SomeoneElse), k_words.clone(), 1).expect("Kit can add Ana to the second town");
        a.pair(PairSide::Join, a_words.clone(), 1).expect("Ana can pair to join Kit's town");
        k.link_proved(&ak, VERSION, "Ana", Some(&k_words)).unwrap();
        a.link_proved(&kk, VERSION, "Kit", Some(&a_words)).unwrap();
        exchange(&mut a, &mut k, Some(&kit_hello));
        a.open_town(town).unwrap();
        k.open_town(town).unwrap();
        a.link_proved(&bk, VERSION, "Ben", None).unwrap();
        b.link_proved(&ak, VERSION, "Ana", None).unwrap();
        exchange(&mut a, &mut b, None);
        b.open_town(town).unwrap();

        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); makepad_create::script_mod(vm); });
        let cx = &mut *pool;
        let (mut ana, a_ports) = attach_call_seat(cx, a, &[(bk, "Ben"), (kk, "Kit")]);
        let (mut ben, b_ports) = attach_call_seat(cx, b, &[(ak, "Ana")]);
        let (mut kit, k_ports) = attach_call_seat(cx, k, &[(ak, "Ana")]);
        let (ab, ba, ak_port, ka) = (&a_ports[&bk], &b_ports[&ak], &a_ports[&kk], &k_ports[&ak]);
        let links = [(ab, ba), (ak_port, ka)];
        let model = ana.model.as_ref().unwrap();
        assert!([bk, kk].iter().all(|p| model.sync_compatible(*p) && model.peer_multiparty(p)), "both peers can sync and take a town call");
        assert!(model.peer_multiparty_share(&bk) && !model.peer_multiparty_share(&kk), "only Ben can take a town share");
        assert_eq!(model.linked_town_peers(town).into_iter().collect::<BTreeSet<_>>(), BTreeSet::from([bk, kk]), "Ana can call both members");
        assert_eq!(model.linked_town_share_peers(town), [bk], "Ben is the only share recipient");
        assert!(model.peer_update(&kk).is_none() && model.home_notice().is_none(), "Ana needs no update for Kit's older greeting");
        assert!(ana.view.call.can_call && ana.view.call.can_share, "the shared town enables Call and Share");

        ana.call.press(cx, Press::Call);
        pump(cx, &mut [&mut ana, &mut ben, &mut kit], &links);
        let call = ben.call.ringing_session().expect("Ana rings Ben");
        assert_eq!(kit.call.ringing_session(), Some(call), "the same call rings Kit");
        kit.call.press(cx, Press::Answer);
        pump(cx, &mut [&mut ana, &mut ben, &mut kit], &links);
        for app in [&mut ana, &mut kit] {
            assert_eq!((app.call.view().phase, app.call.active_call_town()), (Phase::Live, Some(town)), "Ana and Kit are live in the shared town");
            app.record_call_facts(cx);
            assert_eq!(app.model.as_ref().unwrap().call_town(None), Some(town), "the recorded voice fact pins the shared town");
        }
        ana.call.press(cx, Press::HangUp);
        pump(cx, &mut [&mut ana, &mut ben, &mut kit], &links);
        for app in [&mut ana, &mut kit] {
            app.record_call_facts(cx);
            assert_eq!(app.call.view().phase, Phase::Idle, "HangUp ends the answered call");
            assert!(app.model.as_ref().unwrap().call_town(None).is_none(), "Leave clears the active voice call");
        }
        assert_eq!(ben.call.view().phase, Phase::Idle, "HangUp cancels Ben's unanswered ring");

        let (session, open_id) = ([21; 16], [22; 16]);
        let offer = |session, town| Message::Share { session, title: "Fab".into(), w: 640., h: 480.,
            ext: ShareOfferExt { town, ..Default::default() } }.encode();
        ba.messages_in.try_send(offer(session, Some(town))).unwrap();
        ben.call.poll(cx);
        let errors = ben.process_share_events(cx, true);
        assert!(errors.is_empty(), "tweaks must resolve the offer's town before Open syncs: {errors:?}");
        let shown = ben.call.shown();
        let incoming = shown.share.offer(session).expect("Ben receives Ana's offer");
        assert_eq!((incoming.town, incoming.role), (town, ShareRole::Viewer { sharer: ak }), "the offer names its town and authenticated sharer");
        assert!(ben.model.as_ref().unwrap().state.session(&session).is_none(), "no signed Open has synced");
        assert_eq!(ben.model.as_ref().unwrap().share_offer_town(session), Some(town), "the wire offer pins its town before sync");
        ba.messages_in.try_send(Message::ShareOff { session, reason: ShareEndReason::Stopped }.encode()).unwrap();
        ben.call.poll(cx);
        ben.process_share_events(cx, true);
        assert_eq!(ben.model.as_ref().unwrap().share_offer_town(session), None, "ShareOff removes the pre-sync town pin");

        assert!(ana.record_call_fact(Fact::ShareOpen { session: open_id, title: "Fab".into(), town }), "Ana records the town share Open");
        assert!(ben.record_call_fact(Fact::ShareJoin { session: open_id, sharer: ak, town }), "Ben records the town share Join");
        let model = ana.model.as_ref().unwrap();
        assert_eq!(model.share_offer_town(open_id), Some(town), "Ana's signed Open pins its town");
        assert_eq!(ben.model.as_ref().unwrap().share_offer_town(open_id), Some(town), "Ben's Join pins Ana's town");
        let open = model.state.session(&open_id).and_then(|s| s.open.as_ref()).expect("the signed Open folds into Ana's town");
        let mut seats = [bk, kk].map(|p| model.seat_in_town(town, &p).expect("the recipient has a seat in this town"));
        seats.sort_unstable();
        assert_eq!(open.to, seats, "the Open addresses exactly Ben's and Kit's seats");

        for (id, wire_town) in [([23; 16], None), ([24; 16], Some(town))] {
            ak_port.messages_in.try_send(offer(id, wire_town)).unwrap();
        }
        ana.call.poll(cx);
        assert!(ana.call.shown().share.offers.is_empty(), "neither of Kit's offers is admitted");
        ab.messages_in.try_send(offer(session, Some(town))).unwrap();
        ana.call.poll(cx);
        let shown = ana.call.shown();
        assert_eq!(shown.share.offers.iter().map(|o| (o.session, o.role)).collect::<Vec<_>>(),
            [(session, ShareRole::Viewer { sharer: bk })], "only Ben's town-tagged offer reaches Ana");

        ana.switch_town(cx, town_s3);
        assert!(ana.view.call.linked && ana.view.call.can_call && !ana.view.call.can_share, "Kit's town permits voice and disables Share");
        assert!(!ana.call.opening_from_ui(CallAction::Press(Press::Share(ShareChoice::Menu))), "the share picker stays closed in Kit's town");

        let press = CallAction::OnShare(session, Press::LetControl(bk));
        assert!(!ana.call.call_actions(cx, press) && ana.call.opening_from_ui(press), "only the consented opening path may grant control");
        ana.view.page = Page::Live(Live::Share(session));
        let uid = WidgetUid(7);
        let local = |allow_test_home| !matches!(consent::decide(None, allow_test_home), consent::Consent::Refused(_));
        let injected = |allow_test_home| !matches!(consent::decide(Some("test".into()), allow_test_home), consent::Consent::Refused(_));
        assert!(!ana.allow_consented_call_press(press, uid, &[], local), "control needs the captured button");
        assert!(!ana.allow_consented_call_press(press, uid, &[uid], injected), "a verified test home cannot grant injected control");
        assert!(ana.allow_consented_call_press(press, uid, &[uid], local), "a local release on the share page passes the gate");
        assert!(!ana.allow_consented_call_press(CallAction::Press(Press::LetControl(bk)), uid, &[uid], local), "control needs a named share");

        for app in [&mut ana, &mut ben, &mut kit] { app.call.shutdown(cx); }
        while [&ana, &ben, &kit].iter().any(|app| app.call.has_call_workers()) {
            pump(cx, &mut [&mut ana, &mut ben, &mut kit], &links);
            std::thread::yield_now();
        }
        drop((ana, ben, kit));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn member_card_watch_and_open_call_follow_live_roster_and_consent() {
        use crate::TranscriptInput;
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_create::call::{CallEnv, Message, Phase, Press, RingExt, ShareEndReason, ShareOfferExt};
        use makepad_town_core::{event::{Event as TownEvent, Member}, home::Home, share::hello::MediaCaps};
        use std::sync::Arc;

        let ada1 = DeviceKey::generate().unwrap();
        let ada2 = DeviceKey::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("town-member-card-live-{}", makepad_town_core::hex(&ada1.public())));
        let home = Home::open(&dir).unwrap();
        let (founder, _) = DeviceKey::load_or_create(&home.key_path()).unwrap();
        let (town, genesis) = TownEvent::found(&founder, 2, 0, 0, [0; 16]).unwrap();
        let mut log = Log::found(&home.town_log_path(&town), genesis).unwrap();
        let mut add = |key, content| append(&mut log, key, content, None);
        let seat = add(&founder, Content::Member(Member::Invite { subject: ada1.public(), role: 0 }));
        add(&ada1, Content::Member(Member::Accept { entry: seat }));
        add(&ada1, Content::Profile { name: "Ada".into(), colour: 0 });
        let entry = add(&ada1, Content::Member(Member::AddDevice { seat, device: ada2.public() }));
        add(&ada2, Content::Member(Member::Accept { entry }));
        drop(log);
        home.set_open_town(&town).unwrap();

        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, {use mod.widgets.* View{
                home_page := HomePage{visible: true}
                live_call := LiveCallPage{visible: false} live_share := LiveSharePage{visible: false}
                chat_panel := ChatRail{} tree := TownTreeRail{}
            }});
            WidgetRef::script_from_value(vm, root)
        });
        app.model = Some(Model::open_home(home).unwrap().0);
        app.refresh(cx);
        assert_eq!(app.view.members.seats[1].devices.iter().map(|d| d.key).collect::<Vec<_>>(), [ada1.public(), ada2.public()]);
        app.call.configure(CallEnv { files: Default::default(), devices_path: dir.join("devices"),
            voice: VoiceSource::Synthetic { tone_hz: 700. }, signal: Arc::new(|| {}), spawn: std_spawn(), jobs: std_spawn() });
        let port = app.call.link_up(cx, founder.public(), ada2.public(), "Ada").unwrap();
        app.call.set_town_peers(cx, BTreeMap::from([(town, BTreeSet::from([ada1.public(), ada2.public()]))]));
        app.call.set_open_town(Some((town, vec![(ada2.public(), "Ada".into())])));
        app.call.set_peer_caps(ada2.public(), MediaCaps(MediaCaps::MULTIPARTY_V1 | MediaCaps::MULTIPARTY_SHARE_V1));
        let receive = |app: &mut App, cx: &mut Cx, msg: Message| {
            port.messages_in.try_send(msg.encode()).unwrap();
            app.call.poll(cx);
            app.show_call(cx);
        };
        let card = |app: &mut App, cx: &mut Cx, member| {
            app.open_member_card(cx, town, member);
            app.update_member_card(cx);
        };
        let click = |app: &mut App, cx: &mut Cx, button: &ButtonRef, injected_by| {
            let event = Event::A11yAction(a11y_tree::ActionRequest { req: 0,
                node: a11y_tree::NodeId::widget(button.widget_uid().0),
                action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None });
            let input = TranscriptInput::opening(&event, consent::decide(injected_by, false));
            let opening = app.call_opening_buttons(cx, input);
            let mut actions = cx.capture_actions(|cx| app.ui.handle_event(cx, &event, &mut Scope::with_data(&mut app.view)));
            app.consented_call_presses(cx, &event, &mut actions, &opening);
            app.shell_actions(cx, &actions);
        };
        let ada = MemberId::Seat(seat);
        let session = [5; 16];
        let drawing = || Message::Share { session, title: "Drawing".into(), w: 640., h: 480., ext: ShareOfferExt { town: Some(town), ..Default::default() } };
        let watch = app.ui.button(cx, ids!(member_watch));
        let open_call = app.ui.button(cx, ids!(member_open_call));
        app.open_page(cx, Page::Home);
        receive(&mut app, cx, drawing());
        card(&mut app, cx, ada);
        assert!(watch.visible() && !open_call.visible());
        assert_eq!(watch.text(), "Watch Drawing", "the seat can watch its second computer's share");
        click(&mut app, cx, &watch, Some("test".into()));
        assert_eq!(app.view.page, Page::Live(Live::Share(session)));
        assert_eq!(app.view.call.share.watching, None, "an unverified press only navigates");
        app.open_page(cx, Page::Home);
        click(&mut app, cx, &watch, None);
        assert_eq!(app.view.page, Page::Live(Live::Share(session)));
        assert_eq!(app.view.call.share.watching, Some(session), "the local release subscribes to Drawing");
        receive(&mut app, cx, Message::ShareOff { session, reason: ShareEndReason::Stopped });
        app.open_page(cx, Page::Home);
        assert!(!watch.visible() && watch.action_data().unwrap().is::<()>());

        receive(&mut app, cx, Message::Ring { call_id: [6; 16], secret: [0; 32], video: false,
            ext: RingExt { town: Some(town), ..Default::default() } });
        assert!(open_call.visible() && !watch.visible(), "the incoming caller's card offers Open call");
        card(&mut app, cx, MemberId::You);
        assert!(!open_call.visible(), "You have not answered");
        card(&mut app, cx, ada);
        assert!(!app.call_opening_buttons(cx, Some(TranscriptInput::Choose)).contains(&open_call.widget_uid()));
        click(&mut app, cx, &open_call, None);
        assert_eq!(app.view.page, Page::Live(Live::Call));
        assert_eq!(app.call.view().phase, Phase::Ringing, "Open call leaves Answer to its own release");
        app.call.press(cx, Press::Answer);
        app.show_call(cx);
        assert_eq!(app.call.view().phase, Phase::Live);
        app.open_page(cx, Page::Home);
        for member in [MemberId::You, ada] {
            card(&mut app, cx, member);
            assert!(open_call.visible(), "both participants can open the live call");
        }

        receive(&mut app, cx, drawing());
        assert!(watch.visible() && open_call.visible());
        let remove = app.ui.widget(cx, ids!(remove_member));
        let actions = press(cx, remove);
        assert!(app.member_card_actions(cx, &actions) && app.removing.is_some());
        app.update_member_card(cx);
        for button in [&watch, &open_call] {
            assert!(!button.visible() && button.action_data().unwrap().is::<()>(), "removal clears both live actions");
        }
        assert!(!app.call_opening_buttons(cx, Some(TranscriptInput::Choose)).contains(&watch.widget_uid()));
        app.cancel_removal(cx);
        app.call.shutdown(cx);
        while app.call.has_call_workers() { app.call.poll(cx); std::thread::yield_now(); }
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn pump_call_mesh(cx: &mut Cx, apps: &mut [App], ports: &BTreeMap<(usize, usize), makepad_create::link_worker::MediaPort>) {
        loop {
            for app in &mut *apps {
                app.call.poll(cx);
                app.record_call_facts(cx);
            }
            let mut sent = false;
            for (&(i, j), from) in ports {
                for frame in from.messages_out.try_iter() {
                    ports[&(j, i)].messages_in.try_send(frame).expect("deliver the call signal");
                    sent = true;
                }
            }
            if !sent { break }
        }
    }

    fn sync_call_mesh(apps: &mut [App], keys: &[PublicKey], edges: &[(usize, usize)],
        wire_frame: impl Fn(usize, Vec<u8>) -> Vec<u8>) {
        let mut now = std::time::Instant::now();
        loop {
            let mut sent = false;
            let mut wake = None;
            for &(a, b) in edges {
                for (i, j) in [(a, b), (b, a)] {
                    if let Some(frame) = apps[i].model.as_mut().unwrap().pull_sync_at(keys[j], now, &mut wake) {
                        let frame = wire_frame(i, frame);
                        apps[j].model.as_mut().unwrap().receive_from(keys[i], &frame).expect("sync the signed call facts");
                        sent = true;
                    }
                }
            }
            if !sent {
                let Some(deadline) = wake else { break };
                now = deadline;
            }
        }
    }

    #[test]
    fn two_seat_town_call_late_join_and_newcomer_keep_town_scope() {
        use makepad_create::call::{CallScope, Phase, Press};
        use makepad_town_core::link::VERSION;
        const NAMES: [&str; 4] = ["Ana", "Ada", "Ada", "Kit"];
        const ANA: usize = 0;
        const ADA: usize = 1;
        const ADA2: usize = 2;
        const KIT: usize = 3;

        // A words link invites a person or vouches for a sibling. The remaining links
        // prove already shared membership after the signed acceptance has synced.
        fn link(apps: &mut [App], keys: &[PublicKey], from: usize, to: usize, add: Option<AddComputer>) {
            let words = add.map(|choice| {
                let own = Invite { nat: Hint::Easy, addr: "192.0.2.1:4100".parse().unwrap(), lan: None, commit: 1 };
                let peer = Invite { addr: "192.0.2.2:4200".parse().unwrap(), ..own.clone() };
                let from_words = PairWords { own: own.clone(), peer: peer.clone() };
                let to_words = PairWords { own: peer, peer: own };
                let model = apps[from].model.as_mut().unwrap();
                model.pair(PairSide::Add(model.open_id(), choice), from_words.clone(), 1).unwrap();
                apps[to].model.as_mut().unwrap().pair(PairSide::Join, to_words.clone(), 1).unwrap();
                (from_words, to_words)
            });
            for (i, j, contact) in [(from, to, words.as_ref().map(|w| &w.0)), (to, from, words.as_ref().map(|w| &w.1))] {
                let model = apps[i].model.as_mut().unwrap();
                model.link_proved(&keys[j], VERSION, NAMES[j], contact).unwrap();
                apps[i].carriers.up.insert(Attempt { carrier: Carrier::Peer(keys[j]), generation: 1 }, NAMES[j].into());
            }
        }

        fn attach(cx: &mut Cx, apps: &mut [App], keys: &[PublicKey],
            ports: &mut BTreeMap<(usize, usize), makepad_create::link_worker::MediaPort>) {
            for (i, app) in apps.iter_mut().enumerate() {
                app.sync_call_carriers(cx);
                for (attempt, cmd) in app.link_queue.drain(..) {
                    let (Some(Attempt { carrier: Carrier::Peer(peer), .. }), ToWorker::Call(port)) = (attempt, cmd)
                        else { panic!("each newly linked peer gets a call port") };
                    ports.insert((i, keys.iter().position(|&k| k == peer).unwrap()), port);
                }
            }
        }

        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); makepad_create::script_mod(vm); });
        let cx = &mut *pool;
        for town_wide in [true, false] {
            let dir = std::env::temp_dir().join(format!("call-newcomer-{}", makepad_town_core::hex(&DeviceKey::generate().unwrap().public())));
            let mut apps: Vec<_> = (0..4).map(|i| {
                let model = Model::open(&dir.join(i.to_string())).unwrap().0;
                attach_call_seat(cx, model, &[]).0
            }).collect();
            let keys: Vec<_> = apps.iter().map(|app| app.model.as_ref().unwrap().me()).collect();
            let town = apps[ANA].model.as_ref().unwrap().open_id();
            let mut edges = Vec::new();
            for (i, j, choice) in [(ANA, ADA, Some(AddComputer::SomeoneElse)), (ADA, ADA2, Some(AddComputer::Mine)), (ANA, ADA2, None)] {
                link(&mut apps, &keys, i, j, choice);
                edges.push((i, j));
                sync_call_mesh(&mut apps, &keys, &edges, |_, frame| frame);
                apps[j].model.as_mut().unwrap().open_town(town).unwrap();
            }
            let mut ports = BTreeMap::new();
            attach(cx, &mut apps, &keys, &mut ports);
            let callee = apps[ANA].model.as_ref().unwrap().seat_in_town(town, &keys[ADA]).unwrap();
            apps[ANA].call.press(cx, if town_wide { Press::Call } else { Press::CallSeat(callee) });
            pump_call_mesh(cx, &mut apps, &ports);
            let session = apps[ADA].call.ringing_session().expect("Ada's first computer rings");
            assert_eq!(apps[ADA2].call.ringing_session(), Some(session), "Ada's second computer receives the same ring");
            let open = apps[ANA].model.as_ref().unwrap().state.sessions[&session].open.as_ref().unwrap();
            assert_eq!((&open.to[..], open.town_wide), (&[callee][..], town_wide),
                "both kinds of two-seat call sign one callee and their own admission scope");

            apps[ADA2].call.press(cx, Press::Decline);
            pump_call_mesh(cx, &mut apps, &ports);
            sync_call_mesh(&mut apps, &keys, &edges, |_, frame| frame);
            if town_wide {
                let scope = apps[ADA2].model.as_ref().unwrap().call_join_scope(town, session);
                assert_eq!(scope, Some(CallScope::Town), "the declined ring's signed open authorizes a town-wide late join");
                apps[ADA].call.press(cx, Press::Answer);
                apps[ADA2].call.press(cx, Press::JoinCall(session, town, scope.unwrap()));
            } else {
                apps[ADA].call.press(cx, Press::Answer);
            }
            pump_call_mesh(cx, &mut apps, &ports);
            assert_eq!(apps[ANA].call.view().phase, Phase::Live, "Ada's answer establishes the call");
            if town_wide {
                for (i, j) in [(ADA, ADA2), (ADA2, ADA)] {
                    assert!(apps[i].call.voice_peers().contains(&keys[j]), "Ada's computer {i} has a voice link to computer {j}");
                    assert_eq!(apps[i].call.ignored().stale, 0, "Ada's computer {i} accepted the crossed join signals");
                }
            }

            link(&mut apps, &keys, ANA, KIT, Some(AddComputer::SomeoneElse));
            edges.push((ANA, KIT));
            sync_call_mesh(&mut apps, &keys, &edges, |_, frame| frame);
            apps[KIT].model.as_mut().unwrap().open_town(town).unwrap();
            for i in [ANA, ADA, ADA2] {
                assert!(apps[i].model.as_ref().unwrap().state.active_member(&keys[KIT]),
                    "Kit's signed acceptance reached computer {i} before call admission");
            }
            for i in [ADA, ADA2] {
                link(&mut apps, &keys, i, KIT, None);
                edges.push((i, KIT));
            }
            sync_call_mesh(&mut apps, &keys, &edges, |_, frame| frame);
            attach(cx, &mut apps, &keys, &mut ports);
            let kit = apps[KIT].model.as_ref().unwrap();
            let scope = kit.call_join_scope(town, session);
            if town_wide {
                assert_eq!(scope, Some(CallScope::Town), "Kit can join the pre-existing town call");
                apps[KIT].call.press(cx, Press::JoinCall(session, town, scope.unwrap()));
                pump_call_mesh(cx, &mut apps, &ports);
                for i in [ANA, ADA, ADA2] {
                    assert!(apps[KIT].call.voice_peers().contains(&keys[i]), "Kit has a live voice link to computer {i}");
                }
                sync_call_mesh(&mut apps, &keys, &edges, |_, frame| frame);
                for app in &apps {
                    assert!(app.model.as_ref().unwrap().state.participants(&session).contains(&keys[KIT]),
                        "each signed history counts Kit's join");
                }
            } else {
                assert_eq!(scope, None, "Kit cannot join the two-seat card call");
                assert!(kit.active_town_call(town).is_none(), "Kit is never offered that live card call");
            }
            for app in &mut apps { app.call.shutdown(cx); }
            drop(apps);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn call_target_records_seat_on_card_call_and_all_members_on_town_call() {
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_create::{call::{CallAction, CallEnv, Phase, Press}, consent,
            view::{ChatRow, LiveActivity, LocalClock, Workspace}};
        use makepad_town_core::{event::{Event as TownEvent, Member, SessionId}, home::Home, link::VERSION,
            share::{self, hello::{Hello, MediaCaps, MULTIPARTY_CALL_LEVEL}, Message as SyncMessage}};
        use std::sync::Arc;
        const ANA: usize = 0;
        const ADA: usize = 1;
        const ADA2: usize = 2;
        const ANA2: usize = 3;
        const BEN: usize = 4;
        const KIT: usize = 5;
        const OLD: usize = 6;
        const NAMES: [&str; 7] = ["Ana", "Ada", "Ada", "Ana", "Ben", "Kit", "Old"];

        fn hello(i: usize) -> Hello {
            match i {
                KIT => Hello { level: MULTIPARTY_CALL_LEVEL, min_peer: 0, sources: Vec::new(), media_caps: MediaCaps(MediaCaps::MULTIPARTY_V1) },
                OLD => Hello { level: MULTIPARTY_CALL_LEVEL - 1, min_peer: 0, sources: Vec::new(), media_caps: MediaCaps::NONE },
                _ => Hello::local(std::iter::empty()),
            }
        }
        fn connect(model: &mut Model, peer: PublicKey, i: usize) {
            model.pin_linked(&peer, VERSION, NAMES[i]).unwrap();
            model.link_proved(&peer, VERSION, NAMES[i], None).unwrap();
            model.receive_from(peer, &SyncMessage::Hello(hello(i)).encode()).unwrap();
        }
        fn sync(apps: &mut [App], keys: &[PublicKey; 7], edges: &[(usize, usize)]) {
            // Ben reconnects to read history between calls. There is no call port for his offline computer.
            connect(apps[ANA].model.as_mut().unwrap(), keys[BEN], BEN);
            connect(apps[BEN].model.as_mut().unwrap(), keys[ANA], ANA);
            let mut edges = edges.to_vec();
            edges.push((ANA, BEN));
            sync_call_mesh(apps, keys, &edges, |i, frame| {
                if share::is_hello(&frame) { SyncMessage::Hello(hello(i)).encode() } else { frame }
            });
            apps[ANA].model.as_mut().unwrap().end_carrier(keys[BEN], false);
            apps[BEN].model.as_mut().unwrap().end_carrier(keys[ANA], false);
        }
        fn audience(app: &App, session: SessionId) -> &[Id] {
            &app.model.as_ref().unwrap().state.sessions[&session].open.as_ref().unwrap().to
        }
        fn calls(app: &App) -> Vec<makepad_create::view::CallLine> {
            Workspace::read(app.model.as_ref().unwrap(), false, &LocalClock::default()).rows.into_iter()
                .filter_map(|row| if let ChatRow::Call(call) = row { Some(call) } else { None }).collect()
        }

        let dir = std::env::temp_dir().join(format!("call-target-{}", makepad_town_core::hex(&DeviceKey::generate().unwrap().public())));
        let homes: [Home; 7] = std::array::from_fn(|i| Home::open(&dir.join(i.to_string())).unwrap());
        let signers = homes.each_ref().map(|h| DeviceKey::load_or_create(&h.key_path()).unwrap().0);
        let keys = signers.each_ref().map(DeviceKey::public);
        let (town, genesis) = TownEvent::found(&signers[ANA], 2, 0, 0, [0; 16]).unwrap();
        let ana_seat = genesis.id;
        let mut seats = [ana_seat; 7];
        let path = homes[ANA].town_log_path(&town);
        {
            let mut log = Log::found(&path, genesis).unwrap();
            for i in [ADA, BEN, KIT, OLD] {
                let entry = append(&mut log, &signers[ANA], Content::Member(Member::Invite { subject: keys[i], role: 0 }), None);
                append(&mut log, &signers[i], Content::Member(Member::Accept { entry }), None);
                seats[i] = entry;
            }
            for (i, owner) in [(ADA2, ADA), (ANA2, ANA)] {
                seats[i] = seats[owner];
                let entry = append(&mut log, &signers[owner], Content::Member(Member::AddDevice { seat: seats[i], device: keys[i] }), None);
                append(&mut log, &signers[i], Content::Member(Member::Accept { entry }), None);
            }
            for i in 0..keys.len() {
                append(&mut log, &signers[i], Content::Profile { name: NAMES[i].into(), colour: 0 }, None);
            }
        }
        for (i, h) in homes.iter().enumerate() {
            if i != ANA { std::fs::copy(&path, h.town_log_path(&town)).unwrap(); }
            h.set_open_town(&town).unwrap();
        }

        let edges = [(ANA, ADA), (ANA, ADA2), (ANA, ANA2), (ANA, KIT), (ANA, OLD),
            (ADA, ANA2), (ADA, ADA2), (ADA, KIT), (ADA, OLD)];
        let mut pool = test_cx::checkout_test_cx(|vm| { makepad_widgets::script_mod(vm); makepad_create::script_mod(vm); });
        let cx = &mut *pool;
        let mut ports = BTreeMap::new();
        let mut apps: Vec<_> = homes.into_iter().enumerate().map(|(i, home)| {
            let mut app = cx.with_vm(App::script_new);
            let mut model = Model::open_home(home).unwrap().0;
            for &(a, b) in &edges {
                let j = if a == i { b } else if b == i { a } else { continue };
                connect(&mut model, keys[j], j);
                app.carriers.up.insert(Attempt { carrier: Carrier::Peer(keys[j]), generation: 1 }, NAMES[j].into());
            }
            app.call.configure(CallEnv { files: model.home.files(), devices_path: model.home.call_devices_path(),
                voice: if i == ANA { VoiceSource::Synthetic { tone_hz: 440. } } else { VoiceSource::Off },
                spawn: std_spawn(), jobs: std_spawn(), signal: Arc::new(|| {}) });
            app.model = Some(model);
            app.sync_call_carriers(cx);
            app.show_call(cx);
            for (attempt, cmd) in app.link_queue.drain(..) {
                let (Some(Attempt { carrier: Carrier::Peer(peer), .. }), ToWorker::Call(port)) = (attempt, cmd)
                    else { panic!("each compatible carrier receives its call port") };
                ports.insert((i, keys.iter().position(|&k| k == peer).unwrap()), port);
            }
            app
        }).collect();

        let uid = WidgetUid(51);
        for press in [Press::Call, Press::CallSeat(seats[ADA])] {
            let action = CallAction::Press(press);
            assert!(!apps[ANA].allow_consented_call_press(action, uid, &[], |_| true), "local consent requires a captured button");
            assert!(!apps[ANA].allow_consented_call_press(action, uid, &[uid], |_| !matches!(consent::decide(Some("test".into()), false), consent::Consent::Refused(_))),
                "unverified injected input cannot dial");
            assert!(apps[ANA].allow_consented_call_press(action, uid, &[uid], |allow_test_home| matches!(consent::decide(None, allow_test_home), consent::Consent::Local)),
                "a captured local press can dial");
            let actions = cx.capture_actions(|cx| cx.widget_action(uid, action));
            apps[ANA].call_actions(cx, &actions);
            assert_eq!(apps[ANA].call.view().phase, Phase::Idle, "ordinary action dispatch cannot dial even on a synthetic host");
        }

        apps[ANA].call.press(cx, Press::CallSeat(seats[ADA]));
        pump_call_mesh(cx, &mut apps, &ports);
        apps[ANA].show_call(cx);
        let card = apps[ADA].call.ringing_session().expect("a card call rings Ada");
        assert_eq!(apps[ADA2].call.ringing_session(), Some(card), "the other computer on Ada's seat rings too");
        for i in [ADA, ADA2] {
            assert_eq!(apps[ANA].view.live_roster.primary(&keys[i]), Some(&LiveActivity::Ringing), "Ada's computers are shown ringing");
        }
        for i in [ANA2, KIT, OLD, BEN] {
            assert!(apps[i].call.view().phase == Phase::Idle && apps[ANA].view.live_roster.primary(&keys[i]).is_none(),
                "every other computer is idle on the wire and in the roster");
        }
        assert_eq!(audience(&apps[ANA], card), [seats[ADA]], "the signed card call names exactly Ada's seat");
        assert!(!apps[ANA].model.as_ref().unwrap().state.sessions[&card].open.as_ref().unwrap().town_wide,
            "a card call signs seat-only admission");
        apps[ANA].call.press(cx, Press::Cancel);
        pump_call_mesh(cx, &mut apps, &ports);
        sync(&mut apps, &keys, &edges);
        let mut callback_seat = None;
        for i in [ADA, ADA2] {
            let rows = calls(&apps[i]);
            let [row] = rows.as_slice() else { panic!("Ada's computers each see one missed card call") };
            let back = row.call_back.as_ref().expect("Ada can call Ana back");
            assert_eq!((row.id.1, row.words.as_str(), back.seat, back.peer.as_str()), (card, "Missed call from Ana", ana_seat, "Ana"),
                "the callback targets the caller's seat");
            callback_seat = Some(back.seat);
        }
        for i in [BEN, KIT, OLD, ANA2] {
            assert!(apps[i].model.as_ref().unwrap().state.session(&card).is_some() && calls(&apps[i]).is_empty(),
                "a synced card call creates no missed row on a non-callee");
        }

        apps[ADA].call.press(cx, Press::CallSeat(callback_seat.unwrap()));
        pump_call_mesh(cx, &mut apps, &ports);
        let back = apps[ANA].call.ringing_session().expect("the callback rings Ana");
        assert_eq!(apps[ANA2].call.ringing_session(), Some(back), "the callback rings Ana's sibling computer");
        assert_eq!(apps[KIT].call.view().phase, Phase::Idle, "the callback excludes Kit");
        assert_eq!(audience(&apps[ADA], back), [ana_seat], "callback history contains only Ana's seat");
        assert!(!apps[ADA].model.as_ref().unwrap().state.sessions[&back].open.as_ref().unwrap().town_wide,
            "a callback signs seat-only admission");
        apps[ANA].call.press(cx, Press::Answer);
        pump_call_mesh(cx, &mut apps, &ports);
        assert_eq!(apps[ADA].call.view().phase, Phase::Live, "Ana answered Ada's callback");
        apps[ANA].call.press(cx, Press::HangUp);
        pump_call_mesh(cx, &mut apps, &ports);
        apps[ANA2].call.press(cx, Press::Decline);
        pump_call_mesh(cx, &mut apps, &ports);
        sync(&mut apps, &keys, &edges);
        for i in [ADA, ADA2, BEN, KIT, OLD] {
            assert!(apps[i].model.as_ref().unwrap().state.session(&back).is_some() && calls(&apps[i]).is_empty(),
                "the signed connection clears Ada's callback and creates no bystander rows");
        }

        apps[ANA].call.press(cx, Press::Call);
        pump_call_mesh(cx, &mut apps, &ports);
        let wide = apps[ADA].call.ringing_session().expect("the town call rings Ada");
        assert!(apps[ADA2].call.ringing_session() == Some(wide) && apps[KIT].call.ringing_session() == Some(wide),
            "both Ada's computers and Step 3 Kit can take a town call");
        for i in [BEN, OLD] { assert_eq!(apps[i].call.view().phase, Phase::Idle, "offline and legacy members do not ring"); }
        let mut expected = [seats[ADA], seats[BEN], seats[KIT], seats[OLD]];
        expected.sort_unstable();
        assert_eq!(audience(&apps[ANA], wide), expected, "town history includes all other seats regardless of connectivity or build");
        assert!(apps[ANA].model.as_ref().unwrap().state.sessions[&wide].open.as_ref().unwrap().town_wide,
            "a town call signs town-wide admission");
        apps[ADA].call.press(cx, Press::Answer);
        pump_call_mesh(cx, &mut apps, &ports);
        assert_eq!(apps[ANA].call.view().phase, Phase::Live, "Ada answers the town call");
        apps[ADA].call.press(cx, Press::HangUp);
        pump_call_mesh(cx, &mut apps, &ports);
        apps[ANA].call.press(cx, Press::HangUp);
        pump_call_mesh(cx, &mut apps, &ports);
        sync(&mut apps, &keys, &edges);
        for i in [BEN, KIT, OLD] {
            let rows = calls(&apps[i]);
            let [row] = rows.as_slice() else { panic!("an unanswering town member sees exactly one missed call") };
            assert_eq!((row.id.1, row.words.as_str(), row.call_back.as_ref().map(|cb| cb.seat)),
                (wide, "Missed call from Ana", Some(ana_seat)), "the missed town call names its opener across versions and reconnects");
        }
        for i in [ANA2, ADA, ADA2] { assert!(calls(&apps[i]).is_empty(), "the opener and answering seat have no missed call"); }
        drop(apps);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn member_card_call_targets_linked_seat_in_open_town_and_requires_consent() {
        use crate::TranscriptInput;
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_create::call::{decode, CallEnv, Message, Phase};
        use makepad_town_core::{event::{Event as TownEvent, Member}, home::Home, link::VERSION,
            share::{hello::{Hello, MediaCaps}, Message as SyncMessage}};
        use std::sync::Arc;

        let [ada1, ada2, ben, old, kit] = std::array::from_fn(|_| DeviceKey::generate().unwrap());
        let dir = std::env::temp_dir().join(format!("town-member-card-call-{}", makepad_town_core::hex(&ada1.public())));
        let home = Home::open(&dir).unwrap();
        let (founder, _) = DeviceKey::load_or_create(&home.key_path()).unwrap();
        let found = |salt| {
            let (town, genesis) = TownEvent::found(&founder, 2, 0, 0, [salt; 16]).unwrap();
            (town, Log::found(&home.town_log_path(&town), genesis).unwrap())
        };
        let add_seat = |log: &mut Log, key: &DeviceKey, name: &str| {
            let entry = append(log, &founder, Content::Member(Member::Invite { subject: key.public(), role: 0 }), None);
            append(log, key, Content::Member(Member::Accept { entry }), None);
            append(log, key, Content::Profile { name: name.into(), colour: 0 }, None);
            entry
        };
        let (town_a, mut a) = found(1);
        let seat = add_seat(&mut a, &ada1, "Ada");
        let ada_seat = MemberId::Seat(seat);
        let entry = append(&mut a, &ada1, Content::Member(Member::AddDevice { seat, device: ada2.public() }), None);
        append(&mut a, &ada2, Content::Member(Member::Accept { entry }), None);
        let ben_seat = MemberId::Seat(add_seat(&mut a, &ben, "Ben"));
        let old_seat = MemberId::Seat(add_seat(&mut a, &old, "Old"));
        let (town_b, mut b) = found(2);
        add_seat(&mut b, &kit, "Kit");
        drop((a, b));
        home.set_open_town(&town_a).unwrap();

        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, {use mod.widgets.* View{
                home_page := HomePage{visible: true}
                live_call := LiveCallPage{visible: false} live_share := LiveSharePage{visible: false}
                chat_panel := ChatRail{} tree := TownTreeRail{}
            }});
            WidgetRef::script_from_value(vm, root)
        });
        let mut model = Model::open_home(home).unwrap().0;
        for (key, name) in [(&ada1, "Laptop"), (&ada2, "Desktop"), (&ben, "Laptop"), (&old, "Old"), (&kit, "Kit")] {
            model.pin_linked(&key.public(), VERSION, name).unwrap();
        }
        app.model = Some(model);
        app.refresh(cx);
        app.open_page(cx, Page::Home);
        app.call.configure(CallEnv { files: Default::default(), devices_path: dir.join("devices"),
            voice: VoiceSource::Synthetic { tone_hz: 700.0 }, signal: Arc::new(|| {}), spawn: std_spawn(), jobs: std_spawn() });
        let attach = |app: &mut App, key: &DeviceKey, name: &str, generation| {
            let peer = key.public();
            let model = app.model.as_mut().unwrap();
            model.link_proved(&peer, VERSION, name, None).unwrap();
            let hello = if peer == old.public() {
                Hello { level: 3, min_peer: 0, sources: Vec::new(), media_caps: MediaCaps::NONE }
            } else {
                Hello::local(std::iter::empty())
            };
            model.receive_from(peer, &SyncMessage::Hello(hello).encode()).unwrap();
            app.carriers.up.insert(Attempt { carrier: Carrier::Peer(peer), generation }, name.into());
        };
        let detach = |app: &mut App, key: &DeviceKey| {
            let peer = key.public();
            app.carriers.up.remove(&app.carriers.calls[&peer]);
            app.model.as_mut().unwrap().end_carrier(peer, false);
        };
        let sync = |app: &mut App, cx: &mut Cx| {
            app.sync_call_carriers(cx);
            app.show_call(cx);
        };
        let card = |app: &mut App, cx: &mut Cx, member| {
            app.open_member_card(cx, town_a, member);
            app.update_member_card(cx);
        };
        let call = app.ui.button(cx, ids!(member_call));
        let open_call = app.ui.button(cx, ids!(member_open_call));
        let click = |app: &mut App, cx: &mut Cx, injected_by| {
            let event = Event::A11yAction(a11y_tree::ActionRequest { req: 0,
                node: a11y_tree::NodeId::widget(call.widget_uid().0),
                action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None });
            let input = TranscriptInput::opening(&event, consent::decide(injected_by, false));
            let opening = app.call_opening_buttons(cx, input);
            let mut actions = cx.capture_actions(|cx| app.ui.handle_event(cx, &event, &mut Scope::with_data(&mut app.view)));
            app.consented_call_presses(cx, &event, &mut actions, &opening);
            app.consented_member_call(cx, &event, &mut actions, &opening);
            app.shell_actions(cx, &actions);
        };

        attach(&mut app, &old, "Old", 1);
        attach(&mut app, &kit, "Kit", 1);
        sync(&mut app, cx);
        assert_eq!(app.model.as_ref().unwrap().verify_wire_town(town_b, Some(kit.public())), Some(town_b), "Kit is linked in town B");
        card(&mut app, cx, ada_seat);
        assert!(!call.visible(), "a seat with no linked device shows no Call when only legacy and off-town peers are linked");

        attach(&mut app, &ada1, "Laptop", 1);
        sync(&mut app, cx);
        assert!(call.visible(), "Ada's open card shows Call as soon as one of Ada's devices links");
        card(&mut app, cx, ben_seat);
        assert!(!call.visible(), "a seat with no linked device shows no Call while another seat is linked");
        click(&mut app, cx, None);
        assert_eq!(app.call.view().phase, Phase::Idle, "a hidden button cannot dial");
        card(&mut app, cx, old_seat);
        assert!(!call.visible(), "a legacy-only seat shows no Call even when linked");

        card(&mut app, cx, ada_seat);
        detach(&mut app, &ada1);
        attach(&mut app, &ben, "Laptop", 1);
        sync(&mut app, cx);
        assert!(!call.visible(), "replacing Ada's laptop with Ben's identically named laptop hides Call on Ada's open card");
        card(&mut app, cx, ben_seat);
        assert!(call.visible(), "Ben's card shows Call while Ben's laptop is linked");

        attach(&mut app, &ada1, "Laptop", 2);
        attach(&mut app, &ada2, "Desktop", 1);
        sync(&mut app, cx);
        card(&mut app, cx, ada_seat);
        assert!(call.visible() && call.text() == "Call" && !open_call.visible(), "Ada's card keeps Call visible while Ben is also linked");
        card(&mut app, cx, ben_seat);
        assert!(call.visible(), "Ben's card also shows Call while Ada is linked");
        card(&mut app, cx, MemberId::You);
        assert!(!call.visible(), "your own card cannot call a teammate");
        card(&mut app, cx, ada_seat);
        assert!(call.visible(), "Ada's reopened card can start a seat call while Ben is linked");

        detach(&mut app, &ada1);
        detach(&mut app, &ada2);
        sync(&mut app, cx);
        assert!(!call.visible(), "disconnecting Ada's devices hides Call on Ada's open card even while Ben stays linked");
        attach(&mut app, &ada1, "Laptop", 3);
        attach(&mut app, &ada2, "Desktop", 2);
        sync(&mut app, cx);
        assert!(call.visible(), "reconnecting Ada's devices restores Call on Ada's open card while Ben stays linked");
        click(&mut app, cx, Some("test".into()));
        assert_eq!(app.call.view().phase, Phase::Idle, "an unverified press cannot start the call");

        let ports: BTreeMap<_, _> = app.link_queue.drain(..).filter_map(|cmd| match cmd {
            (Some(Attempt { carrier: Carrier::Peer(peer), .. }), ToWorker::Call(port)) => Some((peer, port)),
            _ => None,
        }).collect();
        assert_eq!(app.call.linked_peers().collect::<BTreeSet<_>>(), BTreeSet::from([ada1.public(), ada2.public(), ben.public(), old.public(), kit.public()]),
            "Ada, Ben, Old and Kit all have attached carriers");
        for port in ports.values() { port.messages_out.try_iter().for_each(drop); }
        click(&mut app, cx, None);
        assert_eq!(app.call.view().phase, Phase::Calling, "a local press starts the targeted seat call");
        assert!(!call.visible() && open_call.visible(), "the card switches from Call to Open call");
        let (session, _) = app.model.as_ref().unwrap().active_town_call(town_a).expect("the call is recorded in town A");
        assert_eq!(app.model.as_ref().unwrap().state.sessions[&session].open.as_ref().unwrap().to, [seat],
            "the signed card call names only Ada's seat");
        for peer in [ada1.public(), ada2.public()] {
            let frame = ports[&peer].messages_out.try_recv().expect("each of Ada's devices receives Ring");
            assert!(matches!(decode(&frame), Ok(Some(Message::Ring { call_id, ext, .. }))
                if call_id == session && ext.town == Some(town_a) && ext.target_seat == Some(seat)),
                "both of Ada's devices ring for the targeted seat call");
        }
        for peer in [ben.public(), old.public(), kit.public()] {
            assert!(ports[&peer].messages_out.try_iter().next().is_none(), "Ben, Old and Kit receive no outbound frames from Ada's seat call");
        }
        app.call.shutdown(cx);
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn share_copy_uses_names_without_assigning_pronouns() {
        // Check the actual Rust format strings and Splash copy, before substituting anyone's name.
        for source in [include_str!("connect_window.rs"), include_str!("ui/connect.rs"), include_str!("ui/chat.rs"),
            include_str!("ui/navigation.rs"), include_str!("model.rs"), include_str!("view.rs"), include_str!("ui/pages.rs")] {
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

    #[test]
    fn join_call_button_and_dynamic_let_control_enforce_consent_and_scope() {
        use crate::TranscriptInput;
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_create::call::{CallAction, CallEnv, CallScope, Phase, Press};
        use makepad_town_core::{event::{Event as TownEvent, Member}, home::Home, link::VERSION,
            share::{hello::Hello, Message as SyncMessage}};
        use std::{cell::RefCell, rc::Rc, sync::Arc};
        const NAMES: [&str; 3] = ["Ana", "Ben", "Kit"];
        let dir = std::env::temp_dir().join(format!("town-join-ui-{}", makepad_town_core::hex(&DeviceKey::generate().unwrap().public())));
        let homes = NAMES.map(|n| Home::open(&dir.join(n).join(".makepad/create")).unwrap());
        let signers = homes.each_ref().map(|h| DeviceKey::load_or_create(&h.key_path()).unwrap().0);
        let keys = signers.each_ref().map(DeviceKey::public);
        let (town, genesis) = TownEvent::found(&signers[0], 2, 0, 0, [0; 16]).unwrap();
        let founder = genesis.id;
        let path = homes[0].town_log_path(&town);
        let mut log = Log::found(&path, genesis).unwrap();
        for i in [1, 2] {
            let entry = append(&mut log, &signers[0], Content::Member(Member::Invite { subject: keys[i], role: 0 }), None);
            append(&mut log, &signers[i], Content::Member(Member::Accept { entry }), None);
        }
        for i in 0..3 { append(&mut log, &signers[i], Content::Profile { name: NAMES[i].into(), colour: 0 }, None); }
        drop(log);
        let selected = Rc::new(RefCell::<Option<App>>::new(None));
        let input_app = selected.clone();
        let cx = &mut Cx::new(Box::new(move |cx, event| {
            if let Event::A11yAction(_) = event {
                assert!(cx.injected_by().is_some());
                let mut selected = input_app.borrow_mut();
                let app = selected.as_mut().unwrap();
                let input = TranscriptInput::opening(event, consent::decide(cx.injected_by(), app.call.consent_home.verified()));
                let opening = app.call_opening_buttons(cx, input);
                let mut actions = app.capture_ui_actions(cx, event, false, input);
                app.consented_call_presses(cx, event, &mut actions, &opening);
                app.call_actions(cx, &actions);
            }
        }));
        cx.init_cx_os();
        cx.with_vm(|vm| { makepad_widgets::script_mod(vm); makepad_create::script_mod(vm); });
        let mut ports = BTreeMap::new();
        let mut apps: Vec<_> = homes.into_iter().enumerate().map(|(i, home)| {
            if i != 0 { std::fs::copy(&path, home.town_log_path(&town)).unwrap(); }
            home.set_open_town(&town).unwrap();
            let mut model = Model::open_home(home).unwrap().0;
            let mut app = cx.with_vm(App::script_new);
            app.ui = cx.with_vm(|vm| {
                let root = script_eval!(vm, {use mod.widgets.* View{home_page := HomePage{} project_page := ProjectPage{}
                    live_call := LiveCallPage{} live_share := LiveSharePage{}}});
                WidgetRef::script_from_value(vm, root)
            });
            for j in (0..3).filter(|&j| j != i) {
                model.pin_linked(&keys[j], VERSION, NAMES[j]).unwrap();
                model.link_proved(&keys[j], VERSION, NAMES[j], None).unwrap();
                model.receive_from(keys[j], &SyncMessage::Hello(Hello::local(std::iter::empty())).encode()).unwrap();
                if i != 2 && j != 2 {
                    app.carriers.up.insert(Attempt { carrier: Carrier::Peer(keys[j]), generation: 1 }, NAMES[j].into());
                }
            }
            app.call.configure(CallEnv { files: model.home.files(), devices_path: model.home.call_devices_path(),
                voice: VoiceSource::Synthetic { tone_hz: 440. }, spawn: std_spawn(), jobs: std_spawn(), signal: Arc::new(|| {}) });
            app.model = Some(model);
            app.sync_call_carriers(cx);
            for (attempt, command) in app.link_queue.drain(..) {
                let (Some(Attempt { carrier: Carrier::Peer(p), .. }), ToWorker::Call(port)) = (attempt, command) else { panic!("call port") };
                ports.insert((i, keys.iter().position(|k| *k == p).unwrap()), port);
            }
            app
        }).collect();
        apps[0].call.press(cx, Press::Call);
        pump_call_mesh(cx, &mut apps, &ports);
        let session = apps[1].call.ringing_session().unwrap();
        apps[1].call.press(cx, Press::Answer);
        pump_call_mesh(cx, &mut apps, &ports);
        for (i, j) in [(0, 2), (2, 0), (1, 2), (2, 1)] {
            apps[i].carriers.up.insert(Attempt { carrier: Carrier::Peer(keys[j]), generation: 1 }, NAMES[j].into());
        }
        for (i, app) in apps.iter_mut().enumerate() {
            app.sync_call_carriers(cx);
            for (attempt, command) in app.link_queue.drain(..) {
                let (Some(Attempt { carrier: Carrier::Peer(p), .. }), ToWorker::Call(port)) = (attempt, command) else { panic!("late join port") };
                ports.insert((i, keys.iter().position(|k| *k == p).unwrap()), port);
            }
        }
        sync_call_mesh(&mut apps, &keys, &[(0, 1), (0, 2), (1, 2)], |_, f| f);
        let kit = &mut apps[2];
        kit.show_call(cx);
        kit.refresh(cx);
        let active = kit.view.active_town_call.as_ref().unwrap();
        assert_eq!((active.session, active.scope, active.can_join), (session, CallScope::Town, true));
        let join = CallAction::Press(Press::JoinCall(session, town, CallScope::Town));
        let opening = kit.call_opening_buttons(cx, Some(TranscriptInput::Choose));
        for page in [ids!(home_page), ids!(project_page)] {
            let page = kit.ui.widget(cx, page);
            page.set_visible(cx, true);
            draw_home_widget(cx, &page, &mut kit.view);
            let button = page.button(cx, ids!(call_join));
            assert!(button.visible() && opening.contains(&button.widget_uid()));
            assert_eq!(button.action_data().as_ref().and_then(|d| d.downcast_ref::<CallAction>()), Some(&join));
        }
        let button = kit.ui.button(cx, ids!(home_join_call.call_join));
        let uid = button.widget_uid();
        let request = a11y_tree::ActionRequest { req: 1, node: a11y_tree::NodeId::widget(uid.0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None };
        let event = Event::A11yAction(request.clone());
        assert!(!kit.allow_consented_call_press(join, uid, &[], |_| true));
        let mut actions = cx.capture_actions(|cx| kit.ui.handle_event(cx, &event, &mut Scope::with_data(&mut kit.view)));
        kit.call_actions(cx, &actions);
        assert_eq!(kit.call.view().phase, Phase::Idle, "queued Join on a synthetic host cannot dial");
        let callee = kit.model.as_ref().unwrap().seat_in_town(town, &keys[2]).unwrap();
        for stale in [Press::JoinCall(session, [9; 32], CallScope::Town), Press::JoinCall(session, town, CallScope::Seats { caller: founder, callee }),
            Press::JoinCall([9; 16], town, CallScope::Town)] {
            actions = cx.capture_actions(|cx| cx.widget_action(uid, CallAction::Press(stale)));
            kit.consented_call_presses(cx, &event, &mut actions, &opening);
            assert_eq!(kit.call.view().phase, Phase::Idle, "stale Join {stale:?}");
        }

        // Route a driver press through the platform, which stamps its origin for the entire event.
        let window = WindowId(0, 0);
        cx.a11y.set_consumers(a11y_tree::consumer::TEST);
        cx.a11y.begin_frame();
        let root = cx.a11y.begin_node(kit.ui.widget_uid().0, None);
        cx.a11y.begin_window(window);
        let node = cx.a11y.begin_node(uid.0, None);
        let mut semantics = a11y_tree::Node::default();
        button.borrow().unwrap().semantics(cx, &mut semantics);
        cx.a11y.end_node(node, semantics, button.area());
        cx.a11y.end_window();
        cx.a11y.end_node(root, a11y_tree::Node { role: a11y_tree::Role::Window, ..Default::default() }, Area::Empty);
        cx.a11y.finish_frame(Area::Empty, |_| None, |_| Some(a11y_tree::Rect { x: 0., y: 0., w: 100., h: 24. }), |_| None, |_, _| {});
        assert_eq!(cx.a11y.node_window(request.node), Some(window));
        *selected.borrow_mut() = apps.pop();
        cx.dispatch_a11y_action(window, request);
        assert!(cx.injected_by().is_none(), "platform remote-input scope clears on return");
        apps.push(selected.borrow_mut().take().unwrap());
        let kit = &mut apps[2];
        assert_eq!(kit.call.view().phase, Phase::Idle, "injected Join outside a verified test home");

        let local = |allow_test_home| consent::decide(None, allow_test_home) == consent::Consent::Local;
        let unverified_injected = |allow_test_home| !matches!(
            consent::decide(Some("test".into()), allow_test_home && false), consent::Consent::Refused(_));
        let verified_injected = |allow_test_home| matches!(
            consent::decide(Some("test".into()), allow_test_home), consent::Consent::TestDriver(_));
        assert!(!kit.allow_consented_call_press(join, uid, &opening, unverified_injected),
            "unverified injected Join is refused even with a captured button");
        assert!(TranscriptInput::opening(&event, consent::decide(Some("test".into()), true))
            == Some(TranscriptInput::Choose), "a verified test home admits Join's opening release");
        assert!(kit.allow_consented_call_press(join, uid, &opening, verified_injected),
            "a verified test home passes JoinCall's consent gate (allow_test_home = true)");

        // Exercise the real widget -> consented_call_presses -> scope check -> synthetic call_opening_presses path
        // via a local release without mutating process-wide environment variables.
        let mut actions = kit.capture_ui_actions(cx, &event, false, Some(TranscriptInput::Choose));
        kit.consented_call_presses(cx, &event, &mut actions, &opening);
        kit.call_actions(cx, &actions);
        assert_eq!(kit.call.view().phase, Phase::Calling);
        assert!(kit.view.active_town_call.is_none(), "entering the call clears Join");
        pump_call_mesh(cx, &mut apps, &ports);
        for (i, app) in apps.iter().enumerate() {
            assert_eq!(app.call.view().phase, Phase::Live);
            assert_eq!(app.call.voice_peers().len(), 2);
            assert_eq!(app.call.ignored().stale, 0, "{} received stale signals", NAMES[i]);
        }

        // Dynamic LetControl consent: both the primary live_share.let_control (Kit) and a dynamic
        // RequestRow.allow (Ben) require a captured button, matching share page, and local release;
        // injected presses are refused even under a verified test home (opens_right = true).
        let ana = &mut apps[0];
        let share_session = [11; 16];
        let primary_uid = ana.ui.widget(cx, ids!(live_share.let_control)).widget_uid();
        assert_ne!(primary_uid, WidgetUid(0));
        let opening_primary_only = ana.call_opening_buttons(cx, Some(TranscriptInput::Choose));
        assert!(opening_primary_only.contains(&primary_uid), "primary let_control is an opening button");
        let dynamic_uid = WidgetUid(opening_primary_only.iter().map(|u| u.0).max().unwrap_or(0).wrapping_add(1));
        assert!(!opening_primary_only.contains(&dynamic_uid));
        let mut let_opening = opening_primary_only.clone();
        let_opening.push(dynamic_uid);
        ana.view.page = Page::Live(Live::Share(share_session));
        for (label, button_uid, peer) in [("Kit (primary)", primary_uid, keys[2]), ("Ben (dynamic)", dynamic_uid, keys[1])] {
            let press = CallAction::OnShare(share_session, Press::LetControl(peer));
            assert!(!ana.call.call_actions(cx, press) && ana.call.opening_from_ui(press),
                "{label}: ordinary call_actions cannot grant control");
            assert!(!ana.allow_consented_call_press(press, button_uid, &[], local),
                "{label}: uncaptured button cannot grant control");
            assert!(!ana.allow_consented_call_press(press, button_uid, &let_opening, verified_injected),
                "{label}: injected Allow is refused even in a verified test home");
            assert!(ana.allow_consented_call_press(press, button_uid, &let_opening, local),
                "{label}: local release on the open share page passes the gate");
            assert!(!ana.allow_consented_call_press(CallAction::Press(Press::LetControl(peer)), button_uid, &let_opening, local),
                "{label}: unscoped LetControl cannot pass the gate");
            ana.view.page = Page::Live(Live::Share([99; 16]));
            assert!(!ana.allow_consented_call_press(press, button_uid, &let_opening, local),
                "{label}: Allow on another share page is refused");
            ana.view.page = Page::Live(Live::Share(share_session));
        }
        assert!(!ana.allow_consented_call_press(
            CallAction::OnShare(share_session, Press::LetControl(keys[1])), dynamic_uid, &opening_primary_only, local),
            "a removed dynamic RequestRow.allow UID is refused even while primary let_control remains captured");

        for app in &mut apps { app.call.shutdown(cx); }
        while apps.iter().any(|app| app.call.has_call_workers()) {
            for app in &mut apps { app.call.poll(cx); }
            std::thread::yield_now();
        }
        drop(apps);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
