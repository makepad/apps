use super::wire::{self, Body, Packet};
use super::*;
use super::design::session::{validate_design_inputs, BakeInputs, Completed, DesignSession, Origin};
use makepad_town_core::{event::Id, fold::{Agent, Profile, State}};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

pub use makepad_town_core::share::hello::DESIGN_LEVEL;
const MAX_GATED_LEVEL: u16 = DESIGN_LEVEL;
pub const OLDER_SHARER_DESIGN: &str = "The sharer's copy of Makepad Town is older and does not support structural design yet.";
const OLDER_GUEST_DESIGN: &str = "This seat's copy of Makepad Town does not support structural design.";

pub const THEME_LEVEL: u16 = 3;
pub const OLDER_SHARER_THEME: &str = "The sharer's copy of Makepad Town is older and does not support live themes yet.";
const OLDER_GUEST_THEME: &str = "This seat's copy of Makepad Town does not support live themes.";

pub const TREE_LEVEL: u16 = 2;
pub const OLDER_SHARER_TREE: &str = "The sharer's copy of Makepad Town is older and does not support the widget tree yet.";
const OLDER_GUEST_TREE: &str = "This seat's copy of Makepad Town does not support the widget tree.";

#[derive(Clone, Debug, PartialEq)]
pub struct Selection { pub path: String, pub rect: Option<Rect>, pub agent: Option<Id> }

#[derive(Default)]
pub struct SeatView {
    pub(crate) design: design::DesignStateView,
    pub window: Option<(usize, [f64; 2])>,
    pub asked: BTreeSet<Seat>,
    pub allowed: BTreeSet<Seat>,
    pub running: BTreeMap<(Seat, Option<Id>), (RequestId, Request)>,
    pub looking: BTreeMap<(Seat, Id), RequestId>,
    pub(super) finished: BTreeMap<(Seat, Id), (Request, u64)>,
    /// Each verified AI seen in this share, with its last Stop setting.
    pub agents: BTreeMap<(Seat, Id), bool>,
    pub selections: BTreeMap<Seat, Selection>,
    pub(super) selection_revision: u64,
    /// Drain after dispatching to the local UI and held AI calls.
    pub updates: VecDeque<(Seat, RequestId, Option<Id>, Notice)>,
}

impl SeatView {
    fn receive(&mut self, who: Who, notice: Notice) {
        let actor = (who.seat, who.agent);
        let mut done = None;
        if matches!(notice, Notice::Picked { .. } | Notice::Rows { .. } | Notice::Applied { .. } | Notice::Refused(_))
            && self.running.get(&actor).is_some_and(|(req, _)| *req == who.req) {
            let (_, request) = self.running.remove(&actor).unwrap();
            done = match &notice {
                Notice::Picked { path, .. } => Some(Request::Pick(At::Path(path.clone()))),
                Notice::Rows { .. } | Notice::Applied { .. } => Some(request),
                _ => None,
            };
        }
        if let Some(id) = who.agent {
            let ai = (who.seat, id);
            if matches!(notice, Notice::Looked | Notice::Refused(_)) && self.looking.get(&ai) == Some(&who.req) {
                self.looking.remove(&ai);
                if notice == Notice::Looked { done = Some(Request::Look); } else { self.finished.remove(&ai); }
            }
            if done.is_none() {
                if let Notice::Applied { path, prop, new, .. } = &notice {
                    if path == "theme" {
                        // Applied carries the resolved value or install label to every seat.
                        done = Some(Request::Theme(if prop == "install" {
                            ThemeOp::Install(super::ThemeInstall { label: new.clone(), base: String::new(), sheet: None, globals: vec![], overrides: vec![] })
                        } else { ThemeOp::Set { name: prop.clone(), value: new.clone() } }));
                    }
                }
            }
            if let Some(done) = done {
                self.finished.insert(ai, (done, crate::model::wall_ms()));
            } else if matches!(notice, Notice::Running(_))
                || (matches!(notice, Notice::Selected { .. }) && who.req != 0)
            {
                self.finished.remove(&ai);
            }
        }
        match &notice {
            Notice::Running(Request::Look) => {
                if let Some(agent) = who.agent { self.looking.insert((who.seat, agent), who.req); }
            }
            Notice::Running(request) => { self.running.insert(actor, (who.req, request.clone())); }
            Notice::Stopped(stopped) => {
                if let Some(agent) = who.agent { self.agents.insert((who.seat, agent), *stopped); }
            }
            Notice::Window { window, size } => self.window = Some((*window, *size)),
            Notice::Design(DesignNotice::Status(text)) => {
                if let Some(view) = design::DesignStateView::read(text) { self.design = view; }
            }
            Notice::Asked => { self.asked.insert(who.seat); }
            Notice::Allowed(yes) => {
                self.asked.remove(&who.seat);
                if *yes { self.allowed.insert(who.seat); } else { self.allowed.remove(&who.seat); }
            }
            Notice::Picked { path, rect, .. } => {
                for selection in self.selections.values_mut().filter(|s| s.path == *path) {
                    selection.rect = Some(*rect);
                }
                self.selections.insert(who.seat, Selection { path: path.clone(), rect: Some(*rect), agent: who.agent });
            }
            Notice::Selected { path, rect } => match path {
                Some(path) => { self.selections.insert(who.seat, Selection { path: path.clone(), rect: *rect, agent: who.agent }); }
                None => { self.selections.remove(&who.seat); }
            }
            _ => {}
        }
        if matches!(notice, Notice::Picked { .. } | Notice::Selected { .. } | Notice::Design(DesignNotice::Status(_)) | Notice::Allowed(_)) {
            self.selection_revision = self.selection_revision.wrapping_add(1);
        }
        self.updates.push_back((who.seat, who.req, who.agent, notice));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Who { pub(super) seat: Seat, pub(super) req: RequestId, pub(super) agent: Option<Id> }

fn older_sharer(min: u16) -> &'static str {
    match min { DESIGN_LEVEL => OLDER_SHARER_DESIGN, THEME_LEVEL => OLDER_SHARER_THEME, _ => OLDER_SHARER_TREE }
}
fn older_guest(min: u16) -> &'static str {
    match min { DESIGN_LEVEL => OLDER_GUEST_DESIGN, THEME_LEVEL => OLDER_GUEST_THEME, _ => OLDER_GUEST_TREE }
}

/// The app applies a scalar. Keep its validated theme reference for the source implementer.
fn single_theme_ref(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    let rest = trimmed.strip_prefix("theme.")?;
    rest.split('.').all(makepad_town_core::literal::is_identifier).then_some(trimmed)
}

enum WorkKind { Request(Request), ThemeRefresh, DesignRetract }
enum Answer { None, Notice(Notice), BroadcastTheme(String) }

struct Work {
    who: Who,
    kind: WorkKind,
    /// A pick that landed on an unnamed widget, answered if its named ancestor cannot be picked.
    unnamed: Option<Notice>,
}

impl Work {
    fn request(&self) -> Option<&Request> {
        match &self.kind { WorkKind::Request(r) => Some(r), WorkKind::ThemeRefresh | WorkKind::DesignRetract => None }
    }
}

struct Write {
    theme_op: bool,
    install: Option<ThemeInstall>,
    rect: Option<Rect>,
    edit: AppEdit,
    answer: Option<(Who, Notice)>,
}

enum Still<T> { Queued, Capturing(T), Sent(u64) }

struct Owner<L, T> {
    design: DesignSession,
    design_ticket: Option<T>,
    unbaked_props: Arc<BTreeMap<(String, String), String>>,
    last_theme_install: Option<Arc<ThemeInstall>>,
    link: L,
    window: Option<WindowId>,
    allowed: BTreeSet<Seat>,
    peer_levels: BTreeMap<Seat, u16>,
    agents: BTreeMap<Id, Agent>,
    profiles: BTreeMap<Seat, Profile>,
    stopped: BTreeMap<(Seat, Id), String>,
    seen: BTreeSet<(Seat, RequestId)>,
    queue: VecDeque<Work>,
    theme: Option<String>,
    theme_label: String,
    // Fold token deltas into the cached JSON only when a new member needs the snapshot.
    theme_deltas: BTreeMap<String, String>,
    running: Option<(T, Work)>,
    stills: Vec<(Who, Still<T>)>,
    writes: VecDeque<Write>,
    seq: u64,
    rects: BTreeMap<String, Rect>,
    /// The properties the app listed for each widget path. A Set names one of these or is refused:
    /// the app never lists function-valued props or named children.
    listed: BTreeMap<String, BTreeSet<String>>,
    shader_fns: BTreeMap<String, BTreeSet<String>>,
    /// Only interested streams have a mailbox. Shared encoded broadcasts stay until acked.
    out: BTreeMap<Seat, Mailbox>,
    delivery: u64,
}

#[derive(Default)]
struct Mailbox {
    pending: BTreeMap<u64, Arc<Vec<u8>>>,
    sent: u64,
}

pub struct Session<L = LinkId, T = Ticket> {
    pub share: SessionId,
    pub item: Id,
    pub town: Id,
    pub me: Seat,
    pub sharer: Seat,
    sharer_level: Option<u16>,
    pub view: SeatView,
    owner: Option<Owner<L, T>>,
    ended: Option<String>,
    terminal: bool,
    pub(super) ready: bool,
    requests: BTreeMap<RequestId, Vec<u8>>,
    // A guest keeps both wire requests for retry until the owner settles the original look.
    pending_looks: BTreeMap<RequestId, (Option<Id>, Option<RequestId>)>,
    sent: RequestId,
    next: RequestId,
    received: u64,
    ack: Option<u64>,
}

impl<L: Copy + Eq, T: Copy + Eq> Session<L, T> {
    pub fn peer(town: Id, share: SessionId, item: Id, me: Seat, sharer: Seat) -> Result<Self, String> {
        let mut epoch = [0; 8];
        makepad_network::tls::os_random(&mut epoch).map_err(|e| e.to_string())?;
        Ok(Self { town, share, item, me, sharer, sharer_level: None, owner: None, ended: None, terminal: false, ready: false, view: SeatView::default(),
            requests: BTreeMap::new(), pending_looks: BTreeMap::new(), sent: 0, next: (u64::from_ne_bytes(epoch) as u128) << 64, received: 0, ack: None })
    }

    pub fn owner(town: Id, share: SessionId, item: Id, me: Seat, link: L) -> Result<Self, String> {
        let mut session = Self::peer(town, share, item, me, me)?;
        session.owner = Some(Owner { design: Default::default(), design_ticket: None, unbaked_props: Default::default(), last_theme_install: None, link, window: None, allowed: BTreeSet::new(), agents: BTreeMap::new(), seen: BTreeSet::new(), queue: VecDeque::new(),
            profiles: BTreeMap::new(), stopped: BTreeMap::new(), peer_levels: BTreeMap::new(),
            theme: None, theme_label: String::new(), theme_deltas: Default::default(),
            running: None, stills: Vec::new(), writes: VecDeque::new(), seq: 0, rects: BTreeMap::new(), listed: BTreeMap::new(), shader_fns: BTreeMap::new(), out: BTreeMap::new(), delivery: 0 });
        Ok(session)
    }

    pub(super) fn commit_design_to_disk(&mut self) -> Result<(), String> {
        if self.me != self.sharer || self.is_ended() { return Err("Only the active share owner can commit.".into()) }
        let owner = self.owner.as_mut().ok_or("Only the share owner can commit.")?;
        let result = owner.design.commit_to_disk();
        let status = owner.design.state_view().json().to_json();
        self.emit(Who { seat: self.me, req: 0, agent: None }, Notice::Design(DesignNotice::Status(status)));
        result
    }

    pub fn supports_tree(&self) -> bool {
        self.owner.is_some() || self.sharer_level.is_some_and(|level| level >= TREE_LEVEL)
    }

    pub fn supports_theme(&self) -> bool {
        self.owner.is_some() || self.sharer_level.is_some_and(|level| level >= THEME_LEVEL)
    }

    pub fn supports_design(&self) -> bool {
        self.owner.is_some() || self.sharer_level.is_some_and(|level| level >= DESIGN_LEVEL)
    }

    /// Mailboxes can outlive the board (e.g. a background town unloaded on disconnect).
    pub(super) fn sync_peer_levels(&mut self, level: impl Fn(&Seat) -> Option<u16>) {
        if let Some(owner) = &self.owner {
            let changes: Vec<_> = owner.out.keys().filter_map(|peer| {
                let current = level(peer);
                (owner.peer_levels.get(peer).copied() != current).then_some((*peer, current))
            }).collect();
            for (peer, current) in changes { self.set_peer_level(peer, current); }
        } else {
            self.set_peer_level(self.sharer, level(&self.sharer));
        }
    }

    /// Only a Hello on this carrier establishes its level. Until then, retain level-gated mail for replay.
    pub fn set_peer_level(&mut self, seat: Seat, level: Option<u16>) {
        if let Some(owner) = &mut self.owner {
            if seat == self.me || owner.peer_levels.get(&seat).copied() == level { return }
            let was_design_peer = owner.peer_levels.get(&seat).is_some_and(|&l| l >= DESIGN_LEVEL);
            match level {
                Some(l) => { owner.peer_levels.insert(seat, l); }
                None => { owner.peer_levels.remove(&seat); }
            }
            if !was_design_peer && level.is_some_and(|l| l >= DESIGN_LEVEL) && owner.out.contains_key(&seat) {
                let status = owner.design.state_view().json().to_json();
                self.emit_to(seat, 0, None, Notice::Design(DesignNotice::Status(status)));
            }
            let owner = self.owner.as_mut().unwrap();
            if let Some(level) = level.filter(|l| *l < MAX_GATED_LEVEL) {
                if let Some(out) = owner.out.get_mut(&seat) {
                    out.pending.retain(|_, body| wire::min_level(body) <= level);
                }
                self.refuse_matching(|who, request| {
                    let min = request.min_level();
                    (who.seat == seat && min > level).then(|| older_guest(min))
                });
            }
        } else if seat == self.sharer && self.sharer_level != level {
            self.sharer_level = level;
            if let Some(l) = level.filter(|l| *l < MAX_GATED_LEVEL) { self.drop_incompatible_requests(l); }
        }
    }

    fn drop_incompatible_requests(&mut self, level: u16) {
        let ended = self.is_ended();
        for (req, bytes) in self.requests.extract_if(.., |_, b| wire::min_level(b) > level) {
            if !ended {
                let agent = wire::decode(self.me, &bytes).ok().flatten().and_then(|p| p.agent);
                self.view.receive(Who { seat: self.me, req, agent }, Notice::Refused(older_sharer(wire::min_level(&bytes)).into()));
            }
        }
    }

    /// Supply this town's active device keys when its membership changes. Other towns can
    /// share a computer link; connection alone does not subscribe them to this app.
    pub fn set_members(&mut self, members: impl IntoIterator<Item = Seat>) {
        let Some(owner) = &mut self.owner else { return };
        let active: BTreeSet<_> = members.into_iter().filter(|seat| seat != &self.me).collect();
        let departed: Vec<_> = owner.out.keys().filter(|seat| !active.contains(*seat)).copied().collect();
        owner.out.retain(|seat, _| active.contains(seat));
        for seat in &departed { owner.peer_levels.remove(seat); }
        for seat in departed {
            self.refuse_looks(seat, "This seat left the share.");
            self.refuse_matching(|who, r| (who.seat == seat && matches!(r, Request::Design(_))).then_some("This seat left the share."));
            if self.view.allowed.contains(&seat) { let _ = self.consent(seat, false); }
            self.view.asked.remove(&seat);
            let req = self.req();
            self.emit(Who { seat, req, agent: None }, Notice::Selected { path: None, rect: None });
        }
        let owner = self.owner.as_mut().unwrap();
        for peer in active {
            if owner.out.contains_key(&peer) { continue }
            let mut out = Mailbox::default();
            let mut snapshot = |seat, agent, req, notice| {
                owner.delivery += 1;
                let packet = Packet { share: self.share, sender: self.me, req, agent,
                    body: Body::Notice { seat, delivery: owner.delivery, notice } };
                out.pending.insert(owner.delivery, Arc::new(packet.encode()));
            };
            if let Some((window, size)) = self.view.window { snapshot(self.me, None, 0, Notice::Window { window, size }); }
            for &seat in &self.view.asked { snapshot(seat, None, 0, Notice::Asked); }
            for &seat in &self.view.allowed { snapshot(seat, None, 0, Notice::Allowed(true)); }
            for (&(seat, agent), &stopped) in &self.view.agents { snapshot(seat, Some(agent), 0, Notice::Stopped(stopped)); }
            for (&seat, s) in &self.view.selections {
                snapshot(seat, s.agent, 0, Notice::Selected { path: Some(s.path.clone()), rect: s.rect });
            }
            for (&(seat, agent), (req, request)) in &self.view.running {
                snapshot(seat, agent, *req, Notice::Running(request.clone()));
            }
            for (&(seat, agent), &req) in &self.view.looking {
                snapshot(seat, Some(agent), req, Notice::Running(Request::Look));
            }
            if !owner.peer_levels.get(&peer).is_some_and(|&l| l < THEME_LEVEL) {
                if let Some(theme) = &mut owner.theme {
                    if !owner.theme_deltas.is_empty() {
                        app::patch_cached_theme(theme, &owner.theme_deltas);
                        owner.theme_deltas.clear();
                    }
                    snapshot(self.me, None, 0, Notice::Theme { theme: theme.clone() });
                }
            }
            if owner.peer_levels.get(&peer).is_some_and(|&l| l >= DESIGN_LEVEL) {
                snapshot(self.me, None, 0, Notice::Design(DesignNotice::Status(owner.design.state_view().json().to_json())));
            }
            owner.out.insert(peer, out);
        }
    }

    /// The sharer verifies each guest AI against the agents known in this share's town.
    pub fn set_agents(&mut self, state: &State) {
        if let Some(owner) = &mut self.owner {
            if owner.agents != state.agents { owner.agents.clone_from(&state.agents); }
            if owner.profiles != state.profiles { owner.profiles.clone_from(&state.profiles); }
        }
    }

    fn req(&mut self) -> RequestId { self.next += 1; self.next }

    pub fn request(&mut self, agent: Option<Id>, request: Request) -> Result<RequestId, String> {
        if self.ended.is_some() { return Err("share ended".into()) }
        if self.owner.is_none() && self.sharer_level.is_some_and(|l| l < request.min_level()) {
            return Err(older_sharer(request.min_level()).into())
        }
        request.validate()?;
        let req = self.req();
        let who = Who { seat: self.me, req, agent };
        if self.owner.is_some() {
            self.accept(who, request);
        } else {
            match request {
                Request::Look => { self.pending_looks.insert(req, (agent, None)); }
                Request::LookDone { look, .. } => {
                    let Some((_, done)) = self.pending_looks.get_mut(&look)
                        .filter(|(actor, done)| *actor == agent && done.is_none())
                        else { return Err("This look is no longer in flight.".into()) };
                    *done = Some(req);
                }
                _ => {}
            }
            let packet = Packet { sender: self.me, share: self.share, agent, req, body: Body::Request(request) };
            self.requests.insert(req, packet.encode());
        }
        Ok(req)
    }

    fn emit(&mut self, who: Who, notice: Notice) {
        let owner = self.owner.as_mut().unwrap();
        if matches!(notice, Notice::Refused(_) | Notice::Looked) {
            if let Some(index) = owner.stills.iter().position(|(w, _)| *w == who) {
                if let (_, Still::Sent(delivery)) = owner.stills.swap_remove(index) {
                    if let Some(out) = owner.out.get_mut(&who.seat) { out.pending.remove(&delivery); }
                }
            }
        }
        owner.delivery += 1;
        if !owner.out.is_empty() {
            let packet = Packet { share: self.share, sender: self.me, req: who.req, agent: who.agent,
                body: Body::Notice { seat: who.seat, delivery: owner.delivery, notice: notice.clone() } };
            let body = Arc::new(packet.encode());
            let min = wire::min_level(&body);
            for (peer, out) in &mut owner.out {
                if min == 0 || owner.peer_levels.get(peer).is_none_or(|&l| l >= min) {
                    out.pending.insert(owner.delivery, body.clone());
                }
            }
        }
        if self.ended.is_none() || matches!(notice, Notice::Ended(_) | Notice::Allowed(false) | Notice::Applied { .. } | Notice::Refused(_)
            | Notice::Design(DesignNotice::Status(_) | DesignNotice::Mutated { .. })) {
            self.view.receive(who, notice);
        }
    }

    fn emit_theme(&mut self, theme: String) {
        if !self.is_ended() {
            self.emit(Who { seat: self.me, req: 0, agent: None }, Notice::Theme { theme });
        }
    }

    fn emit_for(&mut self, request: &Request, who: Who, notice: Notice) {
        if request.min_level() > 0 { self.emit_to_seat(who, notice); } else { self.emit(who, notice); }
    }

    fn emit_to_seat(&mut self, who: Who, notice: Notice) {
        if who.seat == self.me {
            if !self.is_ended() || matches!(notice, Notice::Refused(_)) { self.view.receive(who, notice); }
            return
        }
        self.emit_to(who.seat, who.req, who.agent, notice);
    }

    /// A private delivery shares the sequence and retry rules of broadcasts.
    pub(super) fn emit_to(&mut self, seat: Seat, req: RequestId, agent: Option<Id>, notice: Notice) -> Option<u64> {
        let owner = self.owner.as_mut().unwrap();
        let out = owner.out.get_mut(&seat)?;
        owner.delivery += 1;
        let packet = Packet { share: self.share, sender: self.me, req, agent,
            body: Body::Notice { seat, delivery: owner.delivery, notice } };
        let body = packet.encode();
        if owner.peer_levels.get(&seat).is_some_and(|&l| l < wire::min_level(&body)) { return None }
        out.pending.insert(owner.delivery, Arc::new(body));
        Some(owner.delivery)
    }

    /// A person's Allow covers their AIs. The sharer enforces its grants. A viewer uses the
    /// last grants it received to decide whether to request a panel refresh.
    pub(super) fn may_act(&self, seat: &Seat, agent: Option<Id>, request: &Request) -> Result<(), &str> {
        if *request == Request::Look {
            if seat == &self.sharer { return Err(OWN_LOOK) }
            if agent.is_none() { return Err("Only a guest AI can ask to look at a share.") }
        }
        if let Some(agent) = agent {
            if let Some(owner) = &self.owner {
                if let Some(why) = owner.stopped.get(&(*seat, agent)) { return Err(why) }
            } else if self.view.agents.get(&(*seat, agent)) == Some(&true) {
                return Err("The sharer stopped this AI.")
            }
            if *request == Request::Look { return Ok(()) }
        } else if matches!(request, Request::AskEdit | Request::Select(_)) {
            return Ok(())
        }
        if matches!(request, Request::Design(r) if r.is_read_only()) { return Ok(()); }
        let allowed = self.owner.as_ref().map_or(&self.view.allowed, |owner| &owner.allowed);
        if seat == &self.sharer || allowed.contains(seat) { Ok(()) } else { Err("Ask the owner to allow editing this share.") }
    }

    /// The panel label and this computer's current grant. An AI needs that grant to edit or select.
    pub(crate) fn app_list_access(&self, state: &State, agent: Id) -> (String, bool) {
        (overlay::who(state, &self.sharer, None), self.may_act(&self.me, Some(agent), &Request::Select(None)).is_ok())
    }

    fn accept(&mut self, who: Who, request: Request) {
        if let Request::LookDone { look, ok } = request {
            let original = Who { req: look, ..who };
            if who.agent.is_some_and(|agent| self.view.looking.get(&(who.seat, agent)) == Some(&look)) {
                // A guest can confirm success only after its picture was issued.
                if ok && !self.owner.as_ref().unwrap().stills.iter().any(|(w, state)| *w == original && matches!(state, Still::Sent(_))) { return }
                let outcome = if ok { Notice::Looked } else { Notice::Refused("The look did not finish on the guest's computer.".into()) };
                self.emit(original, outcome);
            }
            return
        }
        if !self.owner.as_mut().unwrap().seen.insert((who.seat, who.req)) { return }
        if self.ended.is_some() {
            self.emit_for(&request, who, Notice::Refused("share ended".into()));
            return
        }
        let owner = self.owner.as_ref().unwrap();
        let min = request.min_level();
        if min > 0 && who.seat != self.me
            && !owner.peer_levels.get(&who.seat).is_some_and(|&l| l >= min) {
            self.emit_to_seat(who, Notice::Refused(older_guest(min).into()));
            return
        }
        if who.seat != self.me && who.agent.is_some_and(|id| !owner.agents.get(&id).is_some_and(|a| a.owner == who.seat)) {
            self.emit_for(&request, who, Notice::Refused("The sharer's Town does not know this AI as one of your AIs.".into()));
            return
        }
        // A private read (including the owner's own AI) has no public activity to announce.
        if let Some(agent) = who.agent.filter(|_| !matches!(request, Request::Tree(_) | Request::Theme(ThemeOp::Get))
            && !matches!(&request, Request::Design(r) if r.is_read_only())) {
            if !self.view.agents.contains_key(&(who.seat, agent)) {
                // Announce a verified actor even when their first request is refused or only queued.
                self.emit(Who { req: 0, ..who }, Notice::Stopped(false));
            }
        }
        if let Err(why) = self.may_act(&who.seat, who.agent, &request) {
            self.emit_for(&request, who, Notice::Refused(why.into()));
            return
        }
        let owner = self.owner.as_ref().unwrap();
        match request {
            Request::Design(ref r) => {
                if let Err(why) = validate_design_inputs(r) {
                    self.emit_to_seat(who, Notice::Refused(why));
                } else if matches!(r, DesignRequest::DropTarget { window, .. } if owner.window != Some(WindowId(*window))) {
                    self.emit_to_seat(who, Notice::Refused("That window is outside this share.".into()));
                } else {
                    self.owner.as_mut().unwrap().queue.push_back(Work { who, kind: WorkKind::Request(request), unnamed: None });
                }
            }
            Request::Look => {
                let agent = who.agent.expect("only a guest AI may look");
                let notice = if self.view.looking.contains_key(&(who.seat, agent)) {
                    Notice::Refused("This AI already has a look in flight.".into())
                } else {
                    self.owner.as_mut().unwrap().stills.push((who, Still::Queued));
                    Notice::Running(Request::Look)
                };
                self.emit(who, notice);
            }
            Request::AskEdit => self.emit(who, Notice::Asked),
            Request::Select(path) => {
                let rect = path.as_ref().and_then(|path| owner.rects.get(path)).copied();
                self.emit(who, Notice::Selected { path, rect });
            }
            Request::Pick(At::Point { window, .. }) if owner.window.is_some_and(|w| w.0 != window) => {
                self.emit(who, Notice::Refused("That window is outside this share.".into()));
            }
            Request::Set { path, prop, value } if owner.shader_fns.get(&path).is_some_and(|fns| fns.contains(&prop)) => {
                let notice = Notice::Refused("Shader code is not applied live. Saved a note for the implementer.".into());
                let edit = AppEdit { seat: who.seat, agent: who.agent, path, prop, change: Change::Note(value) };
                self.owner.as_mut().unwrap().writes.push_back(Write { theme_op: false, install: None, rect: None, edit, answer: Some((who, notice)) });
            }
            Request::Set { path, prop, .. } if !owner.listed.get(&path).is_some_and(|rows| rows.contains(&prop)) =>
                self.emit(who, Notice::Refused(format!("{prop} is not one of the properties this widget lists."))),
            _ => {
                if let Request::Tree(query @ (TreeQuery::Search { .. } | TreeQuery::Reveal { .. })) = &request {
                    self.refuse_matching(|actor, queued| {
                        (actor.seat == who.seat && actor.agent == who.agent &&
                            matches!((queued, query), (Request::Tree(TreeQuery::Search { .. }), TreeQuery::Search { .. })
                                | (Request::Tree(TreeQuery::Reveal { .. }), TreeQuery::Reveal { .. }))).then_some("Superseded by a newer tree query.")
                    });
                }
                if matches!(request, Request::Theme(ThemeOp::Install(_))) {
                    self.refuse_matching(|actor, queued| (actor.seat == who.seat && actor.agent == who.agent &&
                        matches!(queued, Request::Theme(ThemeOp::Install(_)))).then_some("Superseded by a newer theme install."));
                }
                self.owner.as_mut().unwrap().queue.push_back(Work { who, kind: WorkKind::Request(request), unnamed: None });
            }
        }
    }

    pub(super) fn consent(&mut self, seat: Seat, yes: bool) -> Result<(), &'static str> {
        if self.ended.is_some() || seat == self.me { return Err("no peer consent for this seat") }
        let owner = self.owner.as_mut().ok_or("only the share owner can allow")?;
        if yes {
            if !owner.out.contains_key(&seat) { return Err("seat is not in this town") }
            owner.allowed.insert(seat);
        } else {
            owner.allowed.remove(&seat);
            self.refuse_matching(|who, req| (who.seat == seat && !matches!(req, Request::Design(r) if r.is_read_only()))
                .then_some("Allow revoked. Ask the owner again."));
        }
        let req = self.req();
        self.emit(Who { seat, req, agent: None }, Notice::Allowed(yes));
        Ok(())
    }

    pub(super) fn stop_agent(&mut self, seat: Seat, agent: Id, stop: bool) -> Result<(), &'static str> {
        if self.is_ended() { return Err("share ended") }
        let owner = self.owner.as_mut().ok_or("only the share owner can stop an AI")?;
        if !self.view.agents.contains_key(&(seat, agent)) && !owner.agents.get(&agent).is_some_and(|a| a.owner == seat) {
            return Err("unknown AI for this seat")
        }
        if owner.stopped.contains_key(&(seat, agent)) == stop { return Ok(()) }
        if stop {
            let sharer = overlay::actor_label(&owner.profiles, &owner.agents, &self.sharer, None);
            let actor = overlay::actor_label(&owner.profiles, &owner.agents, &seat, Some(agent));
            let why = format!("{sharer} stopped {actor}.");
            owner.stopped.insert((seat, agent), why.clone());
            self.refuse_queued(seat, Some(agent), &why);
            if let Some(&req) = self.view.looking.get(&(seat, agent)) {
                self.emit(Who { seat, req, agent: Some(agent) }, Notice::Refused(why));
            }
        } else {
            owner.stopped.remove(&(seat, agent));
        }
        self.emit(Who { seat, req: 0, agent: Some(agent) }, Notice::Stopped(stop));
        Ok(())
    }

    fn refuse_queued(&mut self, seat: Seat, agent: Option<Id>, why: &str) {
        self.refuse_matching(|who, _| (who.seat == seat && (agent.is_none() || who.agent == agent)).then_some(why));
    }

    fn refuse_matching<'a>(&mut self, reason: impl Fn(Who, &Request) -> Option<&'a str>) {
        let owner = self.owner.as_mut().unwrap();
        if let Some((who, req)) = owner.design.current() {
            if let Some(why) = reason(who, &Request::Design(req.clone())) { owner.design.abort(why); }
        }
        if let Some(holder) = owner.design.holder() {
            if reason(holder, &Request::Design(DesignRequest::GhostCancel)).is_some() {
                Self::queue_retract(owner, holder);
            }
        }
        let mut removed = Vec::new();
        owner.queue.retain(|work| {
            if let Some((request, why)) = work.request().and_then(|r| reason(work.who, r).map(|why| (r, why))) {
                removed.push((work.who, request.min_level(), Notice::Refused(why.into())));
                false
            } else { true }
        });
        for (who, min, notice) in removed {
            if min > 0 { self.emit_to_seat(who, notice); } else { self.emit(who, notice); }
        }
    }

    fn queue_retract(owner: &mut Owner<L, T>, holder: Who) {
        if !owner.queue.front().is_some_and(|w| matches!(w.kind, WorkKind::DesignRetract))
            && !owner.design.cleaning() {
            owner.queue.push_front(Work { who: holder, kind: WorkKind::DesignRetract, unnamed: None });
        }
    }

    fn refuse_looks(&mut self, seat: Seat, why: &str) {
        let looks: Vec<_> = self.view.looking.iter().filter(|((s, _), _)| *s == seat)
            .map(|(&(_, agent), &req)| Who { seat, agent: Some(agent), req }).collect();
        for who in looks { self.emit(who, Notice::Refused(why.into())); }
    }

    fn cancel_pending_looks(&mut self, why: &str) {
        for (req, (agent, done)) in std::mem::take(&mut self.pending_looks) {
            self.requests.remove(&req);
            if let Some(done) = done { self.requests.remove(&done); }
            self.view.receive(Who { seat: self.me, req, agent }, Notice::Refused(why.into()));
        }
    }

    /// Route the complete G11 event batch first. Captures run independently of the design FIFO.
    pub fn drive_app(&mut self, app: &mut impl AppLink<L, T>) {
        let Some(owner) = &mut self.owner else { return };
        if self.ended.is_some() {
            self.drive_design(|id, window, op| app.design(id, window, op));
            return;
        }
        if let Some(window) = owner.window {
            for (_, still) in &mut owner.stills {
                if matches!(still, Still::Queued) { *still = Still::Capturing(app.png_still(owner.link, window)); }
            }
        } else {
            for (who, _) in std::mem::take(&mut owner.stills) {
                self.emit(who, Notice::Refused("The shared window is unavailable.".into()));
            }
        }
        self.drive_design(|id, window, op| app.design(id, window, op));
    }

    /// For a shared G11 hub whose poll also feeds picture, input and recording consumers.
    pub fn drive_design(&mut self, design: impl FnOnce(L, Option<WindowId>, DesignOp) -> T) {
        let Some(owner) = &mut self.owner else { return };
        if owner.running.is_some() || owner.design_ticket.is_some() || !owner.writes.is_empty() { return }
        loop {
            let owner = self.owner.as_mut().unwrap();
            if let Some((who, _)) = owner.design.current() {
                let result = owner.design.poll();
                self.structural_result(who, result);
                let owner = self.owner.as_mut().unwrap();
                if let Some((window, op)) = owner.design.next_app_op() {
                    owner.design_ticket = Some(design(owner.link, window, op));
                    return;
                }
                if owner.design.current().is_some() || !owner.writes.is_empty() { return }
            }
            if self.ended.is_some() && !self.owner.as_ref().unwrap().queue.front().is_some_and(|w| matches!(w.kind, WorkKind::DesignRetract)) {
                self.finish_end(); return
            }
            let Some(work) = self.owner.as_mut().unwrap().queue.pop_front() else { return };
            let who = work.who;
            if matches!(work.kind, WorkKind::DesignRetract) {
                let owner = self.owner.as_mut().unwrap();
                if owner.design.holder() == Some(who) {
                    let result = owner.design.begin(who, DesignRequest::GhostCancel, owner.window, BakeInputs::default(), Origin::Cleanup);
                    self.structural_result(who, result);
                }
                continue;
            }
            let request = work.request();
            if let Some(request) = request {
                if let Err(why) = self.may_act(&who.seat, who.agent, request) {
                    self.emit_for(request, who, Notice::Refused(why.into()));
                    continue
                }
                let min = request.min_level();
                if who.seat != self.me && self.owner.as_ref().unwrap().peer_levels.get(&who.seat).is_some_and(|&l| l < min) {
                    self.emit_to_seat(who, Notice::Refused(older_guest(min).into()));
                    continue
                }
            }
            let owner = self.owner.as_mut().unwrap();
            if let Some(Request::Design(req)) = request {
                let origin = if who.seat == self.me && who.agent.is_none() { Origin::Owner } else { Origin::Participant };
                let announce = who.agent.is_some() && !req.is_read_only();
                let inputs = if matches!(req, DesignRequest::Bake { .. }) {
                    BakeInputs::snapshot(&owner.unbaked_props, owner.last_theme_install.as_ref(), owner.rects.keys().next())
                } else { BakeInputs::default() };
                let result = owner.design.begin(who, req.clone(), owner.window, inputs, origin);
                if announce && result.is_ok() {
                    self.emit(who, Notice::Design(DesignNotice::Running { summary: "Editing the shared app".into() }));
                }
                self.structural_result(who, result);
                continue;
            }
            let get = Request::Theme(ThemeOp::Get);
            let (window, op) = app::design(request.unwrap_or(&get), owner.window).expect("only app requests enter the FIFO");
            let running = request.filter(|r| r.min_level() == 0).map(|r| Notice::Running(r.clone()));
            owner.running = Some((design(owner.link, window, op), work));
            if let Some(notice) = running { self.emit(who, notice); }
            break
        }
    }

    fn structural_result(&mut self, who: Who, result: Result<Option<Completed>, String>) {
        match result {
            Err(why) => self.emit_to_seat(who, Notice::Refused(why)),
            Ok(Some(Completed::CleanupFailed(why))) => {
                // Preserve the snapshot while the share is open so the next mutation retries
                // settlement. The owner must see a failed cleanup even when the holder left.
                self.emit_to_seat(Who { seat: self.me, agent: None, req: 0 },
                    Notice::Refused(format!("Could not restore the drag preview: {why}")));
            }
            Ok(Some(Completed::GhostStatus { reply, .. })) => {
                let status = self.owner.as_ref().unwrap().design.state_view().json().to_json();
                self.emit(Who { seat: self.me, agent: None, req: 0 }, Notice::Design(DesignNotice::Status(status.clone())));
                if reply {
                    self.emit_to_seat(who, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, json: status }));
                }
            }
            Ok(Some(Completed::Reply(kind, v))) =>
                self.emit_to_seat(who, Notice::Design(DesignNotice::Reply { kind, json: v.to_json() })),
            Ok(Some(Completed::Mutation { op, path, focus_path, old, new })) => {
                let owner = self.owner.as_mut().unwrap();
                owner.seq += 1;
                let seq = owner.seq;
                let edit = AppEdit { seat: who.seat, agent: who.agent, path: path.clone(), prop: format!("design:{op}"),
                    change: Change::Applied { old, new, seq } };
                let notice = Notice::Design(DesignNotice::Mutated { op: op.into(), path, focus_path, seq, summary: op.into() });
                owner.writes.push_back(Write { edit, install: None, rect: None, theme_op: false, answer: Some((who, notice)) });
            }
            Ok(None) => {}
        }
    }

    pub(super) fn set_run_dir(&mut self, dir: Option<&str>) {
        if let Some(owner) = &mut self.owner {
            if owner.design.set_run_dir(dir) {
                let status = owner.design.state_view().json().to_json();
                self.emit(Who { seat: self.me, agent: None, req: 0 }, Notice::Design(DesignNotice::Status(status)));
            }
        }
    }

    pub(super) fn configure_design_worker(&mut self, pool: &makepad_widgets::makepad_platform::thread::TaskPool) {
        if let Some(owner) = &mut self.owner { owner.design.configure_worker(pool); }
    }

    #[cfg(test)]
    pub(super) fn set_workspace_root(&mut self, root: std::path::PathBuf) {
        self.owner.as_mut().unwrap().design = DesignSession::with_root(root);
    }

    pub(super) fn link_ended(&mut self, link: L, why: &str) {
        let Some(owner) = self.owner.as_mut().filter(|o| o.link == link) else { return };
        owner.design_ticket = None;
        owner.queue.retain(|w| !matches!(w.kind, WorkKind::DesignRetract));
        let had_ghost = owner.design.holder().is_some();
        let interrupted = owner.design.link_lost();
        let outstanding = owner.running.take();
        if had_ghost && owner.design.holder().is_none() {
            let status = owner.design.state_view().json().to_json();
            self.emit(Who { seat: self.me, req: 0, agent: None }, Notice::Design(DesignNotice::Status(status)));
        }
        if let Some(who) = interrupted { self.emit_to_seat(who, Notice::Refused(why.into())); }
        if let Some((_, work)) = outstanding {
            if let Some(request) = work.request() { self.emit_for(request, work.who, Notice::Refused(why.into())); }
        }
        self.end(why);
        self.finish_end();
    }

    pub(super) fn still_reply(&mut self, link: L, ticket: T, result: &Result<crate::app_link::PngStill, String>) {
        let Some(owner) = self.owner.as_ref().filter(|o| o.link == link) else { return };
        let Some(index) = owner.stills.iter().position(|(_, s)| matches!(s, Still::Capturing(t) if *t == ticket)) else { return };
        let who = owner.stills[index].0;
        let agent = who.agent.expect("only a guest AI can capture a still");
        if self.view.looking.get(&(who.seat, agent)) != Some(&who.req) { return }
        match result {
            Ok(still) => {
                let picture = Notice::Picture { png: still.png.clone(), size: still.size };
                if let Some(delivery) = self.emit_to(who.seat, who.req, Some(agent), picture) {
                    self.owner.as_mut().unwrap().stills[index].1 = Still::Sent(delivery);
                }
            }
            Err(why) => self.emit(who, Notice::Refused(why.clone())),
        }
    }

    pub(super) fn design_reply(&mut self, link: L, ticket: T, ok: bool, json: &str) {
        let Some(owner) = &mut self.owner else { return };
        if owner.link == link && owner.design_ticket == Some(ticket) {
            owner.design_ticket = None;
            let who = owner.design.current().expect("pending design ticket").0;
            let result = owner.design.reply(owner.window, ok, json);
            self.structural_result(who, result);
            self.finish_end();
            return;
        }
        if owner.link != link || !owner.running.as_ref().is_some_and(|(t, _)| *t == ticket) { return }
        let (_, work) = owner.running.take().unwrap();
        let who = work.who;
        let refresh_actor = self.ended.is_none().then_some(Who { seat: self.me, req: 0, agent: None });
        let result = if ok {
            parse_json(json).map_err(|why| {
                if why == TOO_DEEP { log_depth_refusal(who.seat, self.share); }
                why.to_owned()
            })
                .and_then(|v| Self::answer(owner, &work, &v, json, refresh_actor))
        } else { Err(json.into()) };
        match (result, work.request()) {
            (Ok(Answer::Notice(notice)), Some(request)) => self.emit_for(request, who, notice),
            (Ok(Answer::BroadcastTheme(theme)), _) => self.emit_theme(theme),
            (Err(reason), Some(request)) => {
                let note = match request {
                    Request::Set { path, prop, value } => Some((path.as_str(), prop, value, false)),
                    Request::Theme(ThemeOp::Set { name, value }) => Some(("theme", name, value, true)),
                    _ => None,
                };
                if let Some((path, prop, value, theme_op)) = note {
                    let kind = if theme_op { "theme value" } else { "value" };
                    let notice = Notice::Refused(format!("The app did not apply this {kind}: {reason}. Saved a note for the implementer."));
                    let edit = AppEdit { seat: who.seat, agent: who.agent, path: path.into(), prop: prop.clone(), change: Change::Note(value.clone()) };
                    owner.writes.push_back(Write { edit, theme_op, install: None, rect: None, answer: Some((who, notice)) });
                } else if let Some(notice) = &work.unnamed {
                    if let Notice::Picked { path, rect, .. } = notice { owner.rects.insert(path.clone(), *rect); }
                    self.emit(who, notice.clone());
                } else {
                    self.emit_for(request, who, Notice::Refused(reason));
                }
            }
            _ => {} // A failed internal refresh has no requester to settle.
        }
        self.finish_end();
    }

    /// The town refused the signed append. Settle every accepted request before retiring.
    pub(super) fn record_failed(&mut self, why: &str) {
        if let Some(owner) = &mut self.owner {
            let writes = std::mem::take(&mut owner.writes);
            for write in writes {
                if let Some((who, answer)) = write.answer {
                    let mut cleared_ghost = false;
                    if matches!(answer, Notice::Design(DesignNotice::Mutated { .. })) {
                        let design = &mut self.owner.as_mut().unwrap().design;
                        let had_ghost = design.holder().is_some();
                        if design.record_failed(why) { continue; }
                        cleared_ghost = had_ghost && design.holder().is_none();
                    }
                    let notice = Notice::Refused(why.into());
                    if write.theme_op || write.edit.prop.starts_with("design:") { self.emit_to_seat(who, notice); } else { self.emit(who, notice); }
                    if cleared_ghost {
                        let status = self.owner.as_ref().unwrap().design.state_view().json().to_json();
                        self.emit(Who { seat: self.me, agent: None, req: 0 }, Notice::Design(DesignNotice::Status(status)));
                    }
                }
            }
        }
        self.end(why);
        self.finish_end();
    }

    fn answer(owner: &mut Owner<L, T>, work: &Work, v: &makepad_strict_json::Value, json: &str, refresh_actor: Option<Who>) -> Result<Answer, String> {
        let request = work.request();
        if request.is_none() || matches!(request, Some(Request::Theme(ThemeOp::Get))) {
            let label = app::theme_label_of(v);
            if label.is_empty() { return Err("invalid theme snapshot".into()) }
            owner.theme = Some(json.into());
            let custom = ["has_mix", "has_pins"].iter().any(|key| v.get(key).and_then(|v| v.as_bool()) == Some(true));
            if !custom || owner.theme_label.is_empty() { owner.theme_label = label; }
            owner.theme_deltas.clear();
            return Ok(if request.is_none() { Answer::BroadcastTheme(json.into()) }
                else { Answer::Notice(Notice::Theme { theme: json.into() }) })
        }
        let request = request.unwrap();
        Ok(Answer::Notice(match request {
            Request::Tree(query) if app::valid_tree(v) => {
                owner.rects.extend(app::tree_rects(v));
                Notice::Tree { query: query.clone(), tree: json.to_owned() }
            }
            Request::Pick(at) => {
                let (path, ty, rect) = app::picked(v)?;
                // A point lands on the deepest widget, often an unnamed wrapper. People mean the
                // nearest named one, so pick that by its path before answering.
                if let (At::Point { .. }, Some(parent)) = (at, app::named_parent(v)) {
                    owner.queue.push_front(Work { who: work.who, kind: WorkKind::Request(Request::Pick(At::Path(parent))),
                        unnamed: Some(Notice::Picked { path, ty, rect }) });
                    return Ok(Answer::None)
                }
                owner.rects.insert(path.clone(), rect);
                Notice::Picked { path, ty, rect }
            }
            Request::Props { path } if app::rows(v) => {
                owner.listed.insert(path.clone(), app::row_names(v));
                owner.shader_fns.insert(path.clone(), app::shader_fn_names(v));
                Notice::Rows { path: path.clone(), rows: json.to_owned() }
            }
            Request::Set { .. } | Request::Theme(_) => {
                let (path, prop, old, new, refresh, rect) = match request {
                    Request::Set { path, prop, value } => {
                        let (old, reflected) = app::applied(v, prop)?;
                        let rect = app::applied_rect(v).filter(|r| owner.window.is_none_or(|w| w.0 == r.window));
                        (path.as_str(), prop.as_str(), old, single_theme_ref(value).map_or(reflected, str::to_owned), false, rect)
                    }
                    Request::Theme(ThemeOp::Set { name, .. }) if v.get("name").and_then(|v| v.as_str()) == Some(name) => {
                        let (old, new, global) = app::theme_applied(v)?;
                        if global {
                            owner.theme = None;
                            owner.theme_deltas.clear();
                        } else if owner.theme.is_some() && old != new {
                            owner.theme_deltas.insert(name.clone(), new.clone());
                        }
                        ("theme", name.as_str(), old, new, global, None)
                    }
                    Request::Theme(ThemeOp::Install(install)) if v.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
                        let old = std::mem::replace(&mut owner.theme_label, install.label.clone());
                        owner.theme = None;
                        owner.theme_deltas.clear();
                        ("theme", "install", old, install.label.clone(), true, None)
                    }
                    _ => return Err("invalid theme reply".into()),
                };
                if let Some(who) = refresh_actor.filter(|_| refresh && !owner.queue.front().is_some_and(|w| matches!(w.kind, WorkKind::ThemeRefresh))) {
                    owner.queue.push_front(Work { who, kind: WorkKind::ThemeRefresh, unnamed: None });
                }
                owner.seq += 1;
                let seq = owner.seq;
                owner.writes.push_back(Write { theme_op: matches!(request, Request::Theme(_)),
                    install: match request { Request::Theme(ThemeOp::Install(i)) => Some(i.clone()), _ => None },
                    rect,
                    edit: AppEdit { seat: work.who.seat, agent: work.who.agent,
                        path: path.into(), prop: prop.into(), change: Change::Applied { old: old.clone(), new: new.clone(), seq } },
                    answer: Some((work.who, Notice::Applied { path: path.into(), prop: prop.into(), old, new, seq })) });
                return Ok(Answer::None)
            }
            _ => return Err("invalid design reply".into()),
        }))
    }

    /// The front edit stays queued until its signed append succeeds or the town refuses it.
    pub(super) fn next_record(&self) -> Option<crate::model::Action> {
        let write = self.owner.as_ref()?.writes.front()?;
        Some(crate::model::Action::RecordAppEdit { town: self.town, item: self.item, edit: write.edit.clone() })
    }

    /// Acknowledge exactly the front submission, after the signed append succeeds.
    pub(super) fn record_saved(&mut self) {
        if let Some(write) = self.owner.as_mut().and_then(|o| o.writes.pop_front()) {
            if let Some((who, notice)) = write.answer {
                if write.theme_op && matches!(notice, Notice::Refused(_)) { self.emit_to_seat(who, notice); }
                else {
                    let structural = matches!(notice, Notice::Design(DesignNotice::Mutated { .. }));
                    let owner = self.owner.as_mut().unwrap();
                    let mut bake_report = None;
                    let mut rect_update = None;
                    if structural {
                        let (keys, skipped) = owner.design.record_saved();
                        for key in keys { Arc::make_mut(&mut owner.unbaked_props).remove(&key); }
                        bake_report = skipped;
                    } else if let Some(install) = write.install {
                        owner.last_theme_install = Some(Arc::new(install));
                    } else if !write.theme_op {
                        if let Change::Applied { new, .. } = write.edit.change {
                            if let Some(rect) = write.rect {
                                owner.rects.insert(write.edit.path.clone(), rect);
                                rect_update = Some((write.edit.path.clone(), rect));
                            }
                            Arc::make_mut(&mut owner.unbaked_props).insert((write.edit.path, write.edit.prop), new);
                        }
                    }
                    if structural {
                        let status = self.owner.as_ref().unwrap().design.state_view().json().to_json();
                        self.emit(Who { seat: self.me, agent: None, req: 0 }, Notice::Design(DesignNotice::Status(status)));
                    }
                    if let Some(report) = bake_report {
                        self.emit_to_seat(who, Notice::Design(DesignNotice::Reply { kind: DesignReplyKind::State, json: report.to_json() }));
                    }
                    self.emit(who, notice);
                    if !self.is_ended() {
                        if let Some((path, rect)) = rect_update {
                            let seats: Vec<_> = self.view.selections.iter()
                                .filter(|(_, s)| s.path == path && s.rect != Some(rect))
                                .map(|(&seat, s)| (seat, s.agent)).collect();
                            for (seat, agent) in seats {
                                self.emit(Who { seat, req: 0, agent },
                                    Notice::Selected { path: Some(path.clone()), rect: Some(rect) });
                            }
                        }
                    }
                }
            }
        }
        self.finish_end();
    }

    pub(super) fn window(&mut self, window: WindowId, size: [f64; 2]) {
        if self.ended.is_some() || self.view.window == Some((window.0, size)) || !size.iter().all(|n| n.is_finite() && *n > 0.) { return }
        let req = self.req();
        self.emit(Who { seat: self.me, req, agent: None }, Notice::Window { window: window.0, size });
    }

    pub(super) fn bind_window(&mut self, window: WindowId) {
        if let Some(owner) = &mut self.owner { owner.window = Some(window); }
    }

    pub(super) fn source(&self) -> Option<(L, WindowId)> {
        let owner = self.owner.as_ref()?;
        Some((owner.link, owner.window?))
    }

    pub fn is_ended(&self) -> bool { self.ended.is_some() }

    pub(super) fn retired(&self) -> bool {
        self.terminal && self.view.updates.is_empty() && self.ack.is_none() &&
            self.owner.as_ref().is_none_or(|o| o.out.values().all(|m| m.pending.is_empty()))
    }

    fn finish_end(&mut self) {
        if self.terminal || self.owner.is_none() || self.has_unsettled_edits() { return }
        if let Some(reason) = self.ended.clone() {
            let owner = self.owner.as_mut().unwrap();
            if owner.design.holder().is_some() {
                owner.design.clear_ghost();
                let status = owner.design.state_view().json().to_json();
                self.emit(Who { seat: self.me, agent: None, req: 0 }, Notice::Design(DesignNotice::Status(status)));
            }
            self.terminal = true;
            let req = self.req();
            self.emit(Who { seat: self.me, req, agent: None }, Notice::Ended(reason));
        }
    }

    pub fn has_unsettled_edits(&self) -> bool {
        self.owner.as_ref().is_some_and(|o| o.running.is_some() || o.design.current().is_some() || !o.writes.is_empty()
            || o.queue.front().is_some_and(|w| matches!(w.kind, WorkKind::DesignRetract)))
    }

    pub(super) fn unrecorded_edits(&self) -> usize {
        self.owner.as_ref().map_or(0, |owner| {
            let in_flight = owner.running.as_ref().is_some_and(|(_, work)| matches!(work.request(), Some(Request::Set { .. } | Request::Theme(ThemeOp::Set { .. } | ThemeOp::Install(_)))));
            owner.writes.len() + usize::from(in_flight || owner.design.current().is_some_and(|(_, r)| !r.is_read_only()))
        })
    }

    pub fn end(&mut self, why: &str) {
        if self.ended.is_some() { return }
        self.ended = Some(why.into());
        if let Some(owner) = &mut self.owner {
            owner.design.abort(why);
            owner.running.take_if(|(_, w)| matches!(w.kind, WorkKind::ThemeRefresh));
            let allowed = std::mem::take(&mut owner.allowed);
            // A local Leave can precede the final G11 Design event for a Set already applied.
            // Keep its ticket until Design or the terminal G11 Ended event.
            let queue = std::mem::take(&mut owner.queue);
            if let Some(holder) = owner.design.holder() { Self::queue_retract(owner, holder); }
            for seat in allowed {
                let req = self.req();
                self.emit(Who { seat, req, agent: None }, Notice::Allowed(false));
            }
            for work in queue {
                if let Some(request) = work.request() { self.emit_for(request, work.who, Notice::Refused(why.into())); }
            }
            for ((seat, agent), req) in std::mem::take(&mut self.view.looking) {
                self.emit(Who { seat, agent: Some(agent), req }, Notice::Refused(why.into()));
            }
        }
        self.cancel_pending_looks(why);
        self.finish_end();
        self.view.allowed.clear();
        self.view.asked.clear();
        self.view.running.clear();
        self.view.looking.clear();
        self.view.finished.clear();
        self.view.selections.clear();
        // A sent Set may still be committing on the owner. Its result precedes the terminal notice.
        let unsent = self.requests.split_off(&self.sent.saturating_add(1));
        self.refuse_requests(unsent, why);
    }

    fn refuse_requests(&mut self, requests: BTreeMap<RequestId, Vec<u8>>, why: &str) {
        for (req, bytes) in requests {
            let agent = wire::decode(self.me, &bytes).ok().flatten().and_then(|p| p.agent);
            self.view.receive(Who { seat: self.me, req, agent }, Notice::Refused(why.into()));
        }
    }

    pub fn undelivered(&mut self, event: PeerUndelivered) {
        if event.topic == TWEAK && event.count != 0 { self.retry_peer(event.peer); }
    }

    fn retry_peer(&mut self, peer: Seat) {
        if let Some(owner) = &mut self.owner {
            if let Some(out) = owner.out.get_mut(&peer) { out.sent = 0; }
        } else if peer == self.sharer {
            self.sent = 0;
            if self.received != 0 { self.ack = Some(self.received); }
        }
    }

    /// Call on a computer-link loss, including one with no queued outbound bodies.
    pub fn peer_unlinked(&mut self, peer: Seat) {
        self.retry_peer(peer);
        if self.owner.is_some() {
            self.refuse_looks(peer, "The guest disconnected.");
            self.refuse_matching(|who, r| (who.seat == peer && matches!(r, Request::Design(_))).then_some("The guest disconnected."));
        }
        if self.owner.as_ref().is_some_and(|o| o.allowed.contains(&peer)) { let _ = self.consent(peer, false); }
        if self.owner.is_none() && peer == self.sharer {
            self.cancel_pending_looks("The sharer disconnected.");
            self.view.looking.clear();
            self.ready = true;
            self.view.allowed.remove(&self.me);
        }
    }

    fn incoming(&mut self, packet: Packet) -> Result<(), &'static str> {
        let who = Who { seat: packet.sender, req: packet.req, agent: packet.agent };
        match packet.body {
            Body::Request(request) if self.owner.as_ref().is_some_and(|o| o.out.contains_key(&who.seat)) && packet.sender != self.me => self.accept(who, request),
            Body::Ready if self.owner.as_ref().is_some_and(|o| o.out.contains_key(&who.seat)) => self.retry_peer(who.seat),
            Body::Notice { seat, delivery, notice } if self.owner.is_none() && packet.sender == self.sharer => {
                if delivery > self.received {
                    let mut notice = notice;
                    if let Notice::Rows { rows, .. } = &notice {
                        match wire::validate_rows(rows) {
                            Err(TOO_DEEP) => {
                                log_depth_refusal(packet.sender, self.share);
                                notice = Notice::Refused(TOO_DEEP.into());
                            }
                            other => other?,
                        }
                    }
                    let valid = match &notice {
                        Notice::Tree { tree, .. } => wire::validate_tree(tree),
                        Notice::Theme { theme } => wire::validate_theme(theme),
                        Notice::Design(DesignNotice::Status(json) | DesignNotice::Reply { json, .. }) => wire::validate_design_json(json),
                        _ => Ok(()),
                    };
                    if let Err(why) = valid {
                        if why == TOO_DEEP { log_depth_refusal(packet.sender, self.share); }
                        notice = Notice::Refused(why.into());
                    }
                    self.received = delivery;
                    if seat == self.me && matches!(notice, Notice::Looked | Notice::Refused(_)) {
                        if let Some((_, Some(done))) = self.pending_looks.remove(&who.req) { self.requests.remove(&done); }
                    }
                    if seat == self.me && matches!(notice, Notice::Asked | Notice::Selected { .. } | Notice::Picked { .. }
                        | Notice::Rows { .. } | Notice::Tree { .. } | Notice::Theme { .. } | Notice::Applied { .. } | Notice::Refused(_) | Notice::Looked
                        | Notice::Design(DesignNotice::Reply { .. } | DesignNotice::Mutated { .. })) { self.requests.remove(&who.req); }
                    if let Notice::Ended(why) = &notice {
                        self.end(why);
                        self.terminal = true;
                        let pending = std::mem::take(&mut self.requests);
                        self.refuse_requests(pending, why);
                    }
                    if self.ended.is_none() || matches!(notice, Notice::Ended(_) | Notice::Applied { .. } | Notice::Refused(_)
                        | Notice::Design(DesignNotice::Status(_) | DesignNotice::Mutated { .. })) {
                        self.view.receive(Who { seat, ..who }, notice);
                    }
                }
                self.ack = Some(self.received);
            }
            Body::Ack(n) if self.owner.is_some() => {
                if let Some(out) = self.owner.as_mut().unwrap().out.get_mut(&packet.sender) {
                    if n <= out.sent { out.pending.retain(|k, _| *k > n); }
                }
            }
            _ => return Err("unexpected tweak message direction"),
        }
        Ok(())
    }

    fn flush_peer(&mut self, relay: &mut impl Relay, peer: Seat) -> Result<(), NotLinked> {
        if let Some(owner) = &mut self.owner {
            let level = owner.peer_levels.get(&peer).copied();
            let Some(out) = owner.out.get_mut(&peer) else { return Ok(()) };
            if let Some(l) = level.filter(|l| *l < MAX_GATED_LEVEL) {
                out.pending.retain(|_, body| wire::min_level(body) <= l);
            }
            for (&id, body) in out.pending.range((std::ops::Bound::Excluded(out.sent), std::ops::Bound::Unbounded)) {
                // Later deliveries would Ack past an unsent page. Wait for this carrier's Hello.
                let min = wire::min_level(body);
                if min > 0 && !level.is_some_and(|l| l >= min) { break }
                relay.send(peer, TWEAK, body.as_ref().clone())?;
                out.sent = id;
            }
        } else if peer == self.sharer {
            if self.ready {
                let packet = Packet { share: self.share, sender: self.me, req: 0, agent: None, body: Body::Ready };
                relay.send(peer, TWEAK, packet.encode())?;
                self.ready = false;
            }
            if let Some(n) = self.ack {
                let packet = Packet { share: self.share, sender: self.me, req: n.into(), agent: None, body: Body::Ack(n) };
                relay.send(peer, TWEAK, packet.encode())?;
                self.ack = None;
            }
            if self.is_ended() { return Ok(()) }
            if let Some(l) = self.sharer_level.filter(|l| *l < MAX_GATED_LEVEL) { self.drop_incompatible_requests(l); }
            for (&req, bytes) in self.requests.range((std::ops::Bound::Excluded(self.sent), std::ops::Bound::Unbounded)) {
                let min = wire::min_level(bytes);
                if min > 0 && !self.sharer_level.is_some_and(|l| l >= min) { break }
                relay.send(peer, TWEAK, bytes.clone())?;
                self.sent = req;
            }
        }
        Ok(())
    }
}

/// Drain complete bodies once across every share on this topic.
pub fn poll_relay<L: Copy + Eq, T: Copy + Eq>(sessions: &mut [Session<L, T>], relay: &mut impl Relay) -> Vec<&'static str> {
    let mut errors = Vec::new();
    for drop in relay.take_dropped(TWEAK) {
        for session in sessions.iter_mut() { session.undelivered(drop); }
    }
    for (peer, body) in relay.take(TWEAK) {
        let result = wire::decode_envelope(peer, &body).and_then(|packet| {
            let Some(packet) = packet else { return Ok(()) };
            let session = sessions.iter_mut().find(|s| s.share == packet.share).ok_or("unknown tweak share")?;
            session.incoming(packet)
        });
        if let Err(why) = result { errors.push(why); }
    }
    for peer in relay.peers() {
        for session in sessions.iter_mut() {
            if session.flush_peer(relay, peer).is_err() { break }
        }
    }
    errors
}
