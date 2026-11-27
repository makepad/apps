//! The window owns one coordinator. S2 owns share lifecycle and polls G11 exactly once.
use super::*;
use crate::app_link::AppLinks;
use crate::board::canvas::{AppPick, BoardCanvas};
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
    revisions: HashMap<SessionId, usize>,
    selected: Option<SessionId>,
    /// The panel shows only when the person opened it, by picking in the share's picture, or when
    /// someone asks the sharer to edit and the sharer must answer. A share alone never opens it.
    opened: bool,
    painted: Option<(SessionId, usize)>,
    panel: panel::Panel,
    peer_epochs: BTreeMap<Seat, u64>,
    tools: AppTools,
}

impl<L, T> Default for Tweaks<L, T> {
    fn default() -> Self {
        Self { sessions: vec![], opening: vec![], revisions: HashMap::new(),
            selected: None, opened: false, painted: None, panel: Default::default(), peer_epochs: BTreeMap::new(),
            tools: Default::default() }
    }
}

impl<L: Copy + Eq, T: Copy + Eq> Tweaks<L, T> {
    pub fn session(&self, share: SessionId) -> Option<&Session<L, T>> { self.sessions.iter().find(|s| s.share == share) }

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
                    if !self.opened && p.town == model.open_id() { self.selected = Some(p.share); }
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

    /// Revoke consent for departed or replaced links before polling all linked peers.
    pub fn poll_call(&mut self, call: &mut CallController) {
        let epochs = call.peer_epochs().collect();
        for (peer, previous) in std::mem::replace(&mut self.peer_epochs, epochs) {
            if self.peer_epochs.get(&peer) != Some(&previous) {
                self.peer_unlinked(peer);
            }
        }
        self.relay(call);
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
                    if let Some((state, revision)) = model.tweak_board_state(p.town) {
                        session.set_members(state.active_members());
                        session.sync_peer_levels(|peer| model.peer_level(peer));
                        session.set_agents(state);
                        self.revisions.insert(p.share, revision);
                    }
                    if !self.opened && p.town == model.open_id() { self.selected = Some(p.share); }
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
    pub fn tool(&mut self, model: &mut Model, call: &ServiceCall, waits: &mut AgentWaits) -> Option<(Option<ToolResult>, Option<Credential>)> {
        let parsed = app_tool(&call.tool, &call.args)?;
        Some(self.tools.start(&call.call_id, parsed, model, &mut self.sessions, waits))
    }

    pub fn open_panel(&mut self, share: SessionId) -> bool {
        if self.session(share).is_none_or(|s| s.is_ended()) && !self.opening.iter().any(|p| p.share == share) {
            return false
        }
        self.selected = Some(share);
        self.opened = true;
        true
    }

    pub fn close_panel(&mut self) { self.opened = false; }

    pub fn pick(&mut self, share: SessionId, at: At) -> Result<RequestId, String> {
        let s = self.sessions.iter_mut().find(|s| s.share == share).ok_or("The share is unavailable.")?;
        let req = s.request(None, Request::Pick(at))?;
        self.selected = Some(share);
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
        if cache.as_ref().is_some_and(|(id, s)| *id == session.item && s.window == window
            && s.window_size == size && s.revision == Some(revision)) { return false }
        let mut selections = overlay::Selections::new(window, size, &session.view, state);
        selections.revision = Some(revision);
        *cache = Some((session.item, selections));
        true
    }

    pub fn actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        for a in actions.iter().filter_map(|a| a.as_widget_action()) {
            if let Some(pick) = a.action.downcast_ref::<AppPick>() {
                if let Some(s) = self.sessions.iter().find(|s| s.item == pick.item) {
                    if let Err(e) = self.pick(s.share, pick.at.clone()) { log!("Town tweak: {e}"); }
                }
            }
        }
        if let Some(s) = self.sessions.iter_mut().find(|s| Some(s.share) == self.selected) {
            self.panel.actions(cx, &ui.widget(cx, ids!(tweak_panel)), actions, s);
        }
    }

    pub fn local_input(&mut self, cx: &mut Cx, ui: &WidgetRef, event: &Event, actions: &Actions, test_home: bool) {
        if let Some(s) = self.sessions.iter_mut().find(|s| Some(s.share) == self.selected) {
            self.panel.local_input(cx, event, &ui.widget(cx, ids!(tweak_panel)), actions, s,
                |cx| crate::consent::granted(cx, test_home));
        }
    }

    /// Paint received changes. A notice can queue more work after this event's relay and G11 polls.
    pub fn update(&mut self, cx: &mut Cx, ui: &WidgetRef, model: &Model,
        clock: &LocalClock, waits: &mut AgentWaits, mut reply: impl FnMut(ToolResult)) -> BTreeSet<Credential> {
        let mut changed_traces = BTreeSet::new();
        self.sessions.retain(|s| {
            if !s.retired() { return true }
            self.revisions.remove(&s.share);
            false
        });
        let town = model.open_id();
        let waiting = self.opened && self.opening.iter().any(|p| Some(p.share) == self.selected && p.town == town);
        if !waiting && !self.sessions.iter().any(|s| Some(s.share) == self.selected && s.town == town && !s.is_ended()) {
            self.selected = self.sessions.iter().rev().find(|s| s.town == town && !s.is_ended()).map(|s| s.share);
        }
        let selected = self.selected;
        if selected.is_none() && self.painted.is_none() && self.sessions.iter().all(|s| s.view.updates.is_empty()) {
            return changed_traces
        }
        let root = ui.widget(cx, ids!(tweak_panel));
        let board = ui.widget(cx, ids!(canvas.surface));
        let mut received = false;
        let mut asked = None;
        for s in &mut self.sessions {
            let active = selected == Some(s.share);
            let state = model.tweak_board_state(s.town);
            let rev = state.map(|(_, r)| r);
            let changed = active && rev.is_some_and(|r| self.painted != Some((s.share, r)));
            let dirty = changed || !s.view.updates.is_empty();
            let mut canvas_dirty = changed;
            while let Some((seat, req, agent, notice)) = s.view.updates.pop_front() {
                received = true;
                if let Notice::Picture { png, size } = &notice {
                    if seat == s.me {
                        if let Some(agent) = agent {
                            changed_traces.extend(self.tools.picture(s, req, agent, png, *size, waits, &mut reply));
                        }
                    }
                    continue
                }
                if matches!(notice, Notice::Running(_) | Notice::Stopped(_) | Notice::Looked) { continue }
                if notice == Notice::Asked && seat != s.me && s.me == s.sharer && s.town == town && !s.is_ended() {
                    asked = Some(s.share);
                }
                canvas_dirty = true;
                if let Some((result, changed)) = self.tools.answer(s, seat, req, agent, &notice, waits) {
                    changed_traces.extend(changed);
                    reply(result);
                }
                if active {
                    self.panel.notice(cx, &root, s, seat, req, agent, &model.state, &notice);
                }
            }
            if canvas_dirty {
                if let Some(mut canvas) = board.borrow_mut::<BoardCanvas>() {
                    if s.is_ended() { canvas.clear_app_presence(cx, s.item); }
                    else if let (Some((w, size)), Some((state, _))) = (s.view.window, state) {
                        canvas.set_app_presence(cx, s.item, w, size, &s.view, state);
                    }
                }
            }
            if active {
                if dirty { self.panel.refresh(cx, &root, s, &model.state, clock); }
                if changed {
                    self.panel.restore_selection(cx, &root, s);
                    self.panel.sync_history(cx, &root, s, model, None);
                    self.painted = rev.map(|r| (s.share, r));
                }
            }
        }
        if let Some(share) = asked {
            self.selected = Some(share);
            self.opened = true;
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
                if let Some(w) = links.state(id).and_then(|a| a.windows.iter().find(|w| w.id == window)) {
                    s.window(w.id, [w.size.0, w.size.1]);
                }
            }
            s.drive_app(links);
        }
    }
}
