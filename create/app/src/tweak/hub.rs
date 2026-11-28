//! The window owns one coordinator. S2 owns share lifecycle and polls G11 exactly once.
use super::*;
use super::design::{DesignTarget, drag::{DesignPictureEvent, DragController}};
use crate::app_link::AppLinks;
use crate::board::canvas::{AppPick, AppZoom, BoardCanvas};
use crate::call::CallController;
use crate::model::{Action, Model, Origin};
use crate::tools::{app_tool, AgentWaits, AppTools};
use crate::view::LocalClock;
use makepad_ai_services::wire::{ServiceCall, ToolResult};
use makepad_town_core::{entity::field, event::{Id, TownId, SessionMode}, queue::Credential};
use makepad_widgets::{ids, log, Actions, Cx, Event, LiveId, WidgetActionCast, WidgetRef};
use std::collections::{BTreeMap, BTreeSet, HashMap};

struct Opening<L> {
    share: SessionId,
    town: TownId,
    title: String,
    source: Option<(L, WindowId)>,
    board: Option<Id>,
    revision: Option<usize>,
}

pub struct Tweaks<L = LinkId, T = Ticket> {
    pub(super) sessions: Vec<Session<L, T>>,
    opening: Vec<Opening<L>>,
    design_pool: Option<makepad_widgets::makepad_platform::thread::TaskPool>,
    revisions: HashMap<SessionId, usize>,
    control_revisions: HashMap<SessionId, u64>,
    selected: Option<DesignTarget>,
    painted_board: Option<(Id, Id, usize)>,
    /// The person opens the panel with Tweak or a pick on a board's app.
    opened: bool,
    /// The share bar's Select toggle. Opening the panel turns it on: an open panel is select mode.
    selecting: bool,
    painted: Option<(SessionId, usize, bool)>,
    // Capability, pending ask, and named rights can change without a board revision or notice.
    painted_control: Option<(SessionId, bool, bool, Option<u64>)>,
    panel: panel::Panel,
    drag: DragController,
    drag_surface: Option<makepad_widgets::WidgetUid>,
    peer_epochs: BTreeMap<Seat, u64>,
    tools: AppTools,
}

impl<L, T> Default for Tweaks<L, T> {
    fn default() -> Self {
        Self { sessions: vec![], design_pool: None, opening: vec![], revisions: HashMap::new(), control_revisions: HashMap::new(),
            selected: None, painted_board: None, opened: false, selecting: true, painted: None, painted_control: None, panel: Default::default(), drag: Default::default(), drag_surface: None, peer_epochs: BTreeMap::new(),
            tools: Default::default() }
    }
}

impl<L: Copy + Eq, T: Copy + Eq> Tweaks<L, T> {
    pub fn set_themes_dir(&mut self, dir: std::path::PathBuf) { self.panel.set_themes_dir(dir); }

    pub fn session(&self, share: SessionId) -> Option<&Session<L, T>> { self.sessions.iter().find(|s| s.share == share) }

    pub fn panel_share(&self) -> Option<SessionId> {
        match self.selected {
            Some(DesignTarget::LiveApp(share)) if self.opened => Some(share),
            _ => None,
        }
    }

    pub fn sharer_started(&mut self, model: &Model, share: SessionId, title: String, link: L, window: WindowId,
        open_board: Option<Id>) -> Result<(), String> {
        self.start(model, share, title, Some((link, window)), open_board)
    }

    pub fn viewer_started(&mut self, model: &Model, share: SessionId, title: String) -> Result<(), String> {
        self.start(model, share, title, None, None)
    }

    fn start(&mut self, model: &Model, share: SessionId, title: String, source: Option<(L, WindowId)>, open_board: Option<Id>) -> Result<(), String> {
        if self.session(share).is_some() || self.opening.iter().any(|p| p.share == share) { return Ok(()) }
        let town = model.tweak_town(share).ok_or("No shared town for this app.")?;
        let board = open_board.filter(|_| town == model.open_id());
        self.opening.push(Opening { share, town, title, source, board, revision: None });
        Ok(())
    }

    /// Resolve new shares and membership before polling the peer topic for this event.
    pub fn resolve(&mut self, model: &Model) {
        if self.opening.is_empty() && self.sessions.is_empty() { return }
        let me = model.me();
        self.opening.retain_mut(|p| {
            if p.source.is_some() { return true }
            let Some((state, revision)) = model.tweak_board_state(p.town) else { return true };
            if p.revision == Some(revision) { return true }
            p.revision = Some(revision);
            if !state.active_member(&me) { return false }
            let Some(open) = state.session(&p.share).and_then(|s| s.open.as_ref()) else { return true };
            if !matches!(open.mode, SessionMode::Share { .. }) { return false }
            let Some(item) = state.entities.values().filter(|e| !e.flag(field::DELETED) && !e.flag(field::ARCHIVED)
                && share_of(e) == Some((p.share, open.opener))).min_by_key(|e| (e.created, e.id)) else { return true };
            match Session::peer(p.town, p.share, item.id, me, open.opener) {
                Ok(mut session) => {
                    session.ready = true;
                    if !self.opened && p.town == model.open_id() { self.selected = Some(DesignTarget::LiveApp(p.share)); }
                    self.sessions.push(session);
                }
                Err(e) => log!("Town tweak: opening share: {e}"),
            }
            false
        });
        for s in &mut self.sessions {
            s.sync_peer_levels(|peer| model.peer_level(peer));
            if s.is_ended() { continue }
            let Some((state, rev)) = model.tweak_board_state(s.town) else { continue };
            if self.revisions.insert(s.share, rev) == Some(rev) { continue }
            if !state.active_member(&s.me) || !state.active_member(&s.sharer) {
                s.end("A participant left the share's town.");
            } else {
                s.set_members(state.active_members());
                s.sync_peer_levels(|peer| model.peer_level(peer));
                s.set_agents(state);
            }
        }
    }

    pub fn share_ended(&mut self, share: SessionId) {
        self.opening.retain(|p| p.share != share);
        if let Some(s) = self.sessions.iter_mut().find(|s| s.share == share) { s.end("The share ended."); }
    }

    fn ended_link(&mut self, link: L, why: &str) {
        self.opening.retain(|p| !p.source.is_some_and(|(l, _)| l == link));
        for s in &mut self.sessions { s.link_ended(link, why); }
    }

    pub fn peer_unlinked(&mut self, peer: Seat) {
        for s in &mut self.sessions { s.peer_unlinked(peer); }
    }

    /// Apply control changes and handle replaced links before accepting peer requests.
    pub fn poll_call(&mut self, call: &mut CallController) {
        let epochs = call.peer_epochs().collect();
        for (peer, previous) in std::mem::replace(&mut self.peer_epochs, epochs) {
            if self.peer_epochs.get(&peer) != Some(&previous) {
                self.peer_unlinked(peer);
            }
        }
        self.sync_control_rights(call);
        self.panel.asking = call.asking_for_control();
        self.relay(call);
    }

    fn sync_control_rights(&mut self, call: &CallController) {
        for s in &mut self.sessions {
            if s.me != s.sharer || s.is_ended() { continue }
            let revision = call.control_rights_revision(s.share);
            if self.control_revisions.get(&s.share).copied() == revision { continue }
            s.set_control_rights(call.control_rights(s.share).unwrap_or_default());
            if let Some(revision) = revision {
                self.control_revisions.insert(s.share, revision);
            } else {
                self.control_revisions.remove(&s.share);
            }
        }
    }

    pub fn relay(&mut self, relay: &mut impl Relay) {
        for why in poll_relay(&mut self.sessions, relay) { log!("Town tweak: {why}"); }
    }

    pub fn quit(&mut self) {
        self.opening.clear();
        for s in &mut self.sessions { s.end("Town is closing."); }
    }

    pub fn settling(&self) -> bool { self.sessions.iter().any(Session::has_unsettled_edits) }

    /// After quit, count Sets already sent to the app and edits or notes awaiting signed append.
    pub fn unrecorded_edits(&self) -> usize {
        self.sessions.iter().map(Session::unrecorded_edits).sum()
    }

    /// Append on Town's UI thread. A successful signed edit releases its notice to every seat.
    pub fn record(&mut self, model: &mut Model) -> Vec<Id> {
        let mut saved = Vec::new();
        self.opening.retain_mut(|p| {
            let Some((link, window)) = p.source else { return true };
            let action = Action::AddApp { town: p.town, board: p.board, geometry: [0, 0, 320*64, 240*64],
                session: p.share, name: std::mem::take(&mut p.title) };
            match model.act(action, Origin::Ui).and_then(|item| {
                saved.push(item);
                Session::owner(p.town, p.share, item, model.me(), link)
            }) {
                Ok(mut session) => {
                    session.bind_window(window);
                    if let Some(pool) = &self.design_pool { session.configure_design_worker(pool); }
                    if let Some((state, revision)) = model.tweak_board_state(p.town) {
                        session.set_members(state.active_members());
                        session.sync_peer_levels(|peer| model.peer_level(peer));
                        session.set_agents(state);
                        self.revisions.insert(p.share, revision);
                    }
                    if !self.opened && p.town == model.open_id() { self.selected = Some(DesignTarget::LiveApp(p.share)); }
                    self.sessions.push(session);
                }
                Err(why) => log!("Town tweak: opening the App in its share's town: {why}"),
            }
            false
        });
        for session in &mut self.sessions {
            while let Some(action) = session.next_record() {
                match model.act(action, Origin::Ui) {
                    Ok(id) => {
                        saved.push(id);
                        session.record_saved();
                    }
                    Err(why) => {
                        session.record_failed(&format!("The edit could not be recorded in the share's town: {why}"));
                        break
                    }
                }
            }
        }
        saved
    }

    /// A matched tool can update the trace while its reply is still outstanding.
    pub fn tool(&mut self, cx: &mut Cx, ui: &WidgetRef, model: &mut Model, call: &ServiceCall, waits: &mut AgentWaits) -> Option<(Option<ToolResult>, Option<Credential>)> {
        let parsed = app_tool(&call.tool, &call.args)?;
        let dir = self.panel.themes_dir();
        let context = crate::tools::ThemeToolContext { themes_dir: &dir, active_worn: None, mixer_cache: &mut self.panel.mixer_cache };
        let (result, changed, effect) = self.tools.start(&call.call_id, parsed, model, &mut self.sessions, waits, context);
        if let Some(effect) = effect { self.panel.apply_theme_tool_effect(cx, &ui.widget(cx, ids!(tweak_panel)), effect); }
        Some((result, changed))
    }

    pub fn open_panel(&mut self, share: SessionId) -> bool {
        if self.session(share).is_none_or(|s| s.is_ended()) && !self.opening.iter().any(|p| p.share == share) {
            return false
        }
        self.selected = Some(DesignTarget::LiveApp(share));
        self.opened = true;
        self.selecting = true;
        true
    }

    /// Select flips the open panel's share picture between picking widgets and driving the app.
    pub fn toggle_select(&mut self) { self.selecting = !self.selecting; }

    pub fn close_panel(&mut self) {
        self.opened = false;
        if matches!(self.selected, Some(DesignTarget::BoardDesign(_))) { self.selected = None; }
    }

    /// The board's single selection moved to `item` (a design card or nothing). Selecting only
    /// selects: the designer opens through `open_board_design`, and it closes when its design
    /// is no longer the one selected.
    pub fn select_board_design(&mut self, item: Option<Id>) {
        if matches!(self.selected, Some(DesignTarget::BoardDesign(open)) if Some(open) != item) {
            self.selected = None;
            self.opened = false;
        }
    }

    /// The explicit ask to edit a board design: a double-click on its card, or Return on it.
    pub fn open_board_design(&mut self, item: Id) {
        self.selected = Some(DesignTarget::BoardDesign(item));
        self.opened = true;
    }

    pub fn board_design_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions, model: &mut Model) -> Option<Id> {
        let Some(DesignTarget::BoardDesign(item)) = self.selected else { return None };
        if !self.opened { return None }
        self.panel.board_actions(cx, &ui.widget(cx, ids!(tweak_panel)), model, item, actions)
    }

    pub fn pick(&mut self, share: SessionId, at: At) -> Result<RequestId, String> {
        let s = self.sessions.iter_mut().find(|s| s.share == share).ok_or("The share is unavailable.")?;
        let req = s.request(None, Request::Pick(at))?;
        self.selected = Some(DesignTarget::LiveApp(share));
        self.opened = true;
        Ok(req)
    }

    /// Keep the share picture's prepared labels until its selections, town or geometry change.
    pub fn refresh_selections(&self, model: &Model, picture: Option<(SessionId, [f64; 2])>,
        cache: &mut Option<(Id, overlay::Selections)>) -> bool {
        let source = picture.and_then(|(share, size)| {
            let session = self.session(share).filter(|s| !s.is_ended())?;
            let (window, _) = session.view.window?;
            let (state, revision) = model.tweak_board_state(session.town)?;
            Some((session, window, size, state, (revision, session.view.selection_revision)))
        });
        let Some((session, window, size, state, revision)) = source else { return cache.take().is_some() };
        let rest = focus::FocusState::default();
        let focus = self.panel.focus_for(session.share, session.item).unwrap_or(&rest);
        let build = self.opened && self.selected == Some(DesignTarget::LiveApp(session.share)) && self.panel.is_build_mode(session.share, session.item);
        let edit = session.supports_design() && (session.me == session.sharer || session.view.allowed.contains(&session.me));
        let caret = self.drag.caret(session.share);
        let select = (self.opened && self.selected == Some(DesignTarget::LiveApp(session.share))).then_some(self.selecting);
        if cache.as_ref().is_some_and(|(id, s)| *id == session.item && s.share == Some(session.share) && s.window == window
            && s.window_size == size && s.revision == Some(revision) && &s.focus == focus && s.build_mode == build
            && s.can_edit == edit && s.caret.as_ref() == caret && s.select == select) { return false }
        let mut selections = overlay::Selections::new(window, size, &session.view, state).with_focus(focus.clone())
            .with_design(session.share, &session.view, &session.me, build, edit, caret);
        selections.revision = Some(revision);
        selections.select = select;
        *cache = Some((session.item, selections));
        true
    }

    pub fn actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        for a in actions.iter().filter_map(|a| a.as_widget_action()) {
            if let Some(picture) = a.action.downcast_ref::<DesignPictureEvent>() {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.item == picture.item() &&
                    self.selected == Some(DesignTarget::LiveApp(s.share)) && self.opened && !s.is_ended() &&
                    s.supports_design() && (s.me == s.sharer || s.view.allowed.contains(&s.me))) {
                    if let DesignPictureEvent::Resize { resize, .. } = picture {
                        self.panel.resize_widget(cx, &ui.widget(cx, ids!(tweak_panel)), s, resize);
                    } else {
                        if matches!(picture, DesignPictureEvent::Leave { .. }) && self.drag_surface != Some(a.widget_uid) { continue }
                        self.drag_surface = matches!(picture, DesignPictureEvent::Hover { .. }).then_some(a.widget_uid);
                        self.drag.picture(cx, s, picture);
                    }
                }
            }
            if let Some(zoom) = a.action.downcast_ref::<AppZoom>() {
                if let Some(s) = self.sessions.iter().find(|s| s.item == zoom.item && !s.is_ended()) {
                    self.panel.scale_focus_zoom(cx, &ui.widget(cx, ids!(tweak_panel)), s.share, s.item, zoom.factor);
                }
            }
            if let Some(pick) = a.action.downcast_ref::<AppPick>() {
                if let Some(s) = self.sessions.iter().find(|s| s.item == pick.item) {
                    if let Err(e) = self.pick(s.share, pick.at.clone()) { log!("Town tweak: {e}"); }
                }
            }
        }
        if let Some(s) = self.sessions.iter_mut().find(|s| Some(DesignTarget::LiveApp(s.share)) == self.selected) {
            self.panel.actions(cx, &ui.widget(cx, ids!(tweak_panel)), actions, s);
        }
    }

    pub fn handle_event(&mut self, cx: &mut Cx, _ui: &WidgetRef, event: &Event) {
        if matches!(event, Event::DragEnd | Event::WindowLostFocus(_) | Event::KeyDown(_) | Event::Timer(_)) {
            for s in &mut self.sessions { self.drag.event(cx, s, event); }
        }
    }

    pub fn needs_theme_tick(&self) -> bool {
        self.panel.ticking_share().is_some_and(|(share, item)| self.selected == Some(DesignTarget::LiveApp(share))
            && self.sessions.iter().any(|s| s.share == share && s.item == item && !s.is_ended()))
            || self.panel.mixer_cache.ready.is_none() && self.tools.needs_mixer_cache()
    }

    pub fn tick_theme(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event) {
        let Event::NextFrame(frame) = event else { return };
        if self.panel.board_index_frame(cx, self.selected) { self.painted_board = None; }
        if !self.needs_theme_tick() { return }
        let want_cache = self.panel.mixer_cache.ready.is_none()
            && (self.tools.needs_mixer_cache() || self.panel.wants_mixer_cache().is_some_and(|(share, item)| {
                self.selected == Some(DesignTarget::LiveApp(share)) && self.sessions.iter().any(|s| s.share == share && s.item == item && !s.is_ended())
            }));
        if want_cache { self.panel.mixer_cache.step(); }
        self.tools.flush_mixes(&mut self.sessions, &self.panel.mixer_cache);
        if let Some(s) = self.sessions.iter_mut().find(|s| Some(DesignTarget::LiveApp(s.share)) == self.selected) {
            self.panel.tick_theme_at(cx, &ui.widget(cx, ids!(tweak_panel)), s, frame.time);
        }
        if self.needs_theme_tick() { cx.new_next_frame(); }
    }

    pub fn local_input(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event, actions: &Actions, test_home: bool) {
        if let Some(s) = self.sessions.iter_mut().find(|s| Some(DesignTarget::LiveApp(s.share)) == self.selected) {
            self.panel.local_input(cx, event, &ui.widget(cx, ids!(tweak_panel)), actions, s,
                |cx| crate::consent::granted(cx, test_home));
        }
    }

    /// Paint received changes. A notice can queue more work after this event's relay and G11 polls.
    pub fn update(&mut self, cx: &mut Cx, ui: &WidgetRef, model: &Model,
        clock: &LocalClock, waits: &mut AgentWaits, mut reply: impl FnMut(ToolResult)) -> BTreeSet<Credential> {
        let mut changed_traces = BTreeSet::new();
        if self.design_pool.is_none() && !self.sessions.is_empty() {
            let pool = cx.task_pool();
            for s in &mut self.sessions { s.configure_design_worker(&pool); }
            self.design_pool = Some(pool);
        }
        self.sessions.retain(|s| {
            if !s.retired() { return true }
            self.drag.retire(cx, s.share);
            self.revisions.remove(&s.share);
            self.control_revisions.remove(&s.share);
            self.panel.retire_share(s.share, s.item);
            false
        });
        changed_traces.extend(self.tools.take_ready(&self.sessions, waits, &mut reply));
        let town = model.open_id();
        let waiting = self.opened && self.opening.iter().any(|p| Some(DesignTarget::LiveApp(p.share)) == self.selected && p.town == town);
        if !matches!(self.selected, Some(DesignTarget::BoardDesign(_))) && !waiting && !self.sessions.iter().any(|s| Some(DesignTarget::LiveApp(s.share)) == self.selected && s.town == town && !s.is_ended()) {
            self.selected = self.sessions.iter().rev().find(|s| s.town == town && !s.is_ended()).map(|s| DesignTarget::LiveApp(s.share));
        }
        let selected = self.selected;
        let board_revision = match selected {
            Some(DesignTarget::BoardDesign(item)) => Some((town, item, model.open_event_count())),
            _ => None,
        };
        if self.sessions.is_empty() && board_revision.is_some() && board_revision == self.painted_board {
            return changed_traces
        }
        if selected.is_none() && self.painted_board.is_none() && self.painted.is_none() && self.sessions.iter().all(|s| s.view.updates.is_empty()) {
            return changed_traces
        }
        let root = ui.widget(cx, ids!(tweak_panel));
        let board = ui.widget(cx, ids!(canvas.surface));
        let mut received = false;
        for s in &mut self.sessions {
            let active = selected == Some(DesignTarget::LiveApp(s.share));
            let state = model.tweak_board_state(s.town);
            let rev = state.map(|(_, r)| r);
            let changed = active && rev.is_some_and(|r| self.painted != Some((s.share, r, s.supports_design())));
            if !active || !self.opened { self.drag.cancel(cx, s, true); }
            let mut dirty = changed;
            if active {
                let control = (s.share, s.supports_control_tweak(), self.panel.asking == Some(s.share),
                    self.control_revisions.get(&s.share).copied());
                dirty |= self.painted_control != Some(control);
                self.painted_control = Some(control);
            }
            let mut design_status = false;
            while let Some((seat, req, agent, notice)) = s.view.updates.pop_front() {
                received = true;
                if let Notice::Picture { png, size } = &notice {
                    dirty = true;
                    if seat == s.me {
                        if let Some(agent) = agent {
                            changed_traces.extend(self.tools.picture(s, req, agent, png, *size, waits, &mut reply));
                        }
                    }
                    continue
                }
                design_status |= matches!(notice, Notice::Design(DesignNotice::Status(_)));
                if matches!(notice, Notice::Ended(_)) || seat == s.me && matches!(notice, Notice::Stopped(true) | Notice::Allowed(false)) {
                    self.drag.cancel(cx, s, true);
                }
                if seat == s.me && agent.is_none() && self.drag.notice(cx, s, req, &notice) { continue }
                dirty = true;
                if matches!(notice, Notice::Running(_) | Notice::Stopped(_) | Notice::Looked) { continue }
                let dir = self.panel.themes_dir();
                let context = crate::tools::ThemeToolContext { themes_dir: &dir, active_worn: self.panel.worn_for(s.share, s.item), mixer_cache: &mut self.panel.mixer_cache };
                let effect = if let Some((result, changed, effect)) = self.tools.answer(s, seat, req, agent, &notice, waits, context) {
                    changed_traces.extend(changed);
                    reply(result);
                    effect
                } else { None };
                if active {
                    self.panel.notice(cx, &root, s, seat, req, agent, &model.state, &notice);
                } else {
                    self.panel.background_tree_notice(s, seat, req, &notice);
                    if matches!(&notice, Notice::Applied { path, prop, .. } if path == "theme" && prop == "install") {
                        self.panel.clear_worn(cx, &root, s.share, s.item);
                    }
                }
                if let Some(effect) = effect { self.panel.apply_theme_tool_effect(cx, &root, effect); }
            }
            if design_status { self.drag.sync_status(s); }
            let rest = focus::FocusState::default();
            let focus = self.panel.focus_for(s.share, s.item).unwrap_or(&rest);
            if let Some(mut canvas) = board.borrow_mut::<BoardCanvas>() {
                if s.is_ended() { canvas.clear_app_presence(cx, s.item); }
                else if let (Some((window, size)), Some((state, rev))) = (s.view.window, state) {
                    let build = active && self.opened && self.panel.is_build_mode(s.share, s.item);
                    let edit = s.supports_design() && (s.me == s.sharer || s.view.allowed.contains(&s.me));
                    let caret = self.drag.caret(s.share);
                    let revision = Some((rev, s.view.selection_revision));
                    if canvas.app_presence(&s.item).is_none_or(|p| p.revision != revision || p.window != window ||
                        p.window_size != size || p.share != Some(s.share) || &p.focus != focus || p.build_mode != build ||
                        p.can_edit != edit || p.caret.as_ref() != caret) {
                        let mut next = overlay::Selections::new(window, size, &s.view, state).with_focus(focus.clone())
                            .with_design(s.share, &s.view, &s.me, build, edit, caret);
                        next.revision = revision;
                        canvas.set_app_presence(cx, s.item, next);
                    }
                }
            }
            if active {
                if dirty { self.panel.refresh(cx, &root, s, &model.state, clock); }
                if changed {
                    self.panel.restore_selection(cx, &root, s);
                    self.panel.sync_history(cx, &root, s, model, None);
                    self.painted = rev.map(|r| (s.share, r, s.supports_design()));
                }
            }
        }
        if let Some(DesignTarget::BoardDesign(item)) = selected {
            if self.opened && self.painted_board != board_revision {
                self.panel.refresh_board(cx, &root, model, item);
                self.painted_board = board_revision;
            }
            self.painted = None;
        } else {
            self.painted_board = None;
        }
        if self.selected.is_none() {
            self.painted = None;
            self.opened = false;
        }
        root.set_visible(cx, self.opened && !waiting);
        if received { cx.new_next_frame(); }
        changed_traces
    }
}

impl Tweaks {
    pub fn link_events(&mut self, events: &[AppLinkEvent], links: &mut AppLinks) {
        for e in events {
            match e {
                AppLinkEvent::Ended { id, why } => self.ended_link(*id, why),
                AppLinkEvent::Restarted { ended, .. } => self.ended_link(*ended, "The shared app restarted."),
                AppLinkEvent::Design { id, ticket, ok, json } => {
                    for s in &mut self.sessions { s.design_reply(*id, *ticket, *ok, json); }
                }
                AppLinkEvent::PngStill { id, ticket, result } => {
                    for s in &mut self.sessions { s.still_reply(*id, *ticket, result); }
                }
                // LocalInput revokes takeover alone.
                _ => {}
            }
        }
        for s in &mut self.sessions {
            if let Some((id, window)) = s.source() {
                if let Some(link) = links.state(id) {
                    s.set_run_dir(link.app.key.run_dir());
                    if let Some(w) = link.windows.iter().find(|w| w.id == window) {
                        s.window(w.id, [w.size.0, w.size.1]);
                    }
                }
            }
            s.drive_app(links);
        }
    }
}
