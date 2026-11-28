//! One town call at a time across the linked peers. Each input appends its effects in execution order.
use super::signal::{decode, CallId, Message, RingExt, AnswerExt, Why};
use makepad_town_core::{event::{Id, LeaveWhy, DeclineWhy, TownId}, key::PublicKey};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Microseconds to give a person time to answer a ring.
pub const RING_WAIT: u64 = 30_000_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Now {
    pub mono_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallTarget {
    Town,
    Seat(Id),
}

/// The admitted seats of an uninterrupted call, established before opening audio.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallScope {
    Town,
    Seats { caller: Id, callee: Id },
}

impl CallScope {
    fn contains_seat(self, seat: Id) -> bool {
        match self {
            Self::Town => true,
            Self::Seats { caller, callee } => caller == seat || callee == seat,
        }
    }

    fn same_seats(self, other: Self) -> bool {
        match (self, other) {
            (Self::Seats { caller, callee }, Self::Seats { caller: a, callee: b }) =>
                (caller == a && callee == b) || (caller == b && callee == a),
            _ => self == other,
        }
    }

    fn target_seat(self) -> Option<Id> {
        match self { Self::Town => None, Self::Seats { callee, .. } => Some(callee) }
    }

    fn ack_seats(self) -> Option<[Id; 2]> {
        match self { Self::Town => None, Self::Seats { caller, callee } => Some([caller, callee]) }
    }
}

fn ack_scope_matches(scope: CallScope, seats: Option<[Id; 2]>) -> bool {
    match seats {
        None => true,
        Some([caller, callee]) => caller != callee && scope.same_seats(CallScope::Seats { caller, callee }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Press {
    Call,
    CallSeat(Id),
    JoinCall(CallId, TownId, CallScope),
    Answer,
    Decline,
    Cancel,
    HangUp,
    Mute,
    Unmute,
    Transcribe,
    StopTranscribing,
    NotMe,
    Share(super::ShareChoice),
    Watch,
    StopShare,
    CloseWatch,
    Clip,
    Snap,
    StopRecording,
    FirstCaptureWindow,
    LetControl(PublicKey),
    AllowAll(bool),
    RevokeControl(PublicKey),
    TakeBack,
    ReleaseControl,
}

#[derive(Eq, PartialEq)]
#[cfg_attr(test, derive(Debug))]
pub enum Effect {
    Record(Fact),
    SendTo { peer: PublicKey, body: Vec<u8> },
    StartLocalAudio,
    PeerVoice { peer: PublicKey, on: bool },
    StopLocalAudio,
    Mic(bool),
    Speaker(bool),
    Ring(bool),
    Changed,
    Transcribe(bool),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Phase {
    #[default]
    Idle,
    Calling,
    Ringing,
    Live,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fact {
    Open { session: CallId, video: bool, town: TownId, target: CallTarget },
    Join { session: CallId, from: PublicKey, town: TownId },
    Leave { session: CallId, secs: Option<u32>, why: LeaveWhy },
    Decline { session: CallId, why: DeclineWhy, from: PublicKey, town: TownId },
    ShareOpen { session: CallId, title: String, town: TownId },
    ShareJoin { session: CallId, sharer: PublicKey, town: TownId },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ParticipantState {
    pub muted: bool,
    pub transcribing: bool,
    pub joined: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParticipantShown {
    pub peer: PublicKey,
    pub name: String,
    pub muted: bool,
    pub transcribing: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CallView {
    pub phase: Phase,
    pub town: Option<TownId>,
    pub ring_town: Option<TownId>,
    pub participants: Vec<ParticipantShown>,
    pub peer_name: String,
    /// Monotonic microseconds at the start of the current phase.
    pub since: Option<u64>,
    pub duration_secs: u64,
    pub muted: bool,
    pub unmuting: bool,
    pub peer_muted: bool,
    pub transcribing_me: bool,
    pub peer_transcribing: bool,
    pub transcribe_prompt: bool,
}

impl CallView {
    pub fn ringing(&self) -> bool {
        self.phase == Phase::Ringing
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Ignored {
    pub stale: u64,
    pub refused: u64,
}

enum State {
    Idle,
    Calling { call_id: CallId, town: TownId, joining: bool, scope: CallScope, rung: BTreeSet<PublicKey> },
    Ringing {
        call_id: CallId, town: TownId, from: PublicKey, from_is_caller: bool, scope: CallScope,
        roster: BTreeSet<PublicKey>, joined_peers: BTreeMap<PublicKey, ParticipantState>,
    },
    Live {
        call_id: CallId, town: TownId, scope: CallScope, peers: BTreeMap<PublicKey, ParticipantState>,
        ringing: BTreeSet<PublicKey>,
    },
}

fn send(out: &mut Vec<Effect>, peer: PublicKey, message: &Message) {
    out.push(Effect::SendTo { peer, body: message.encode() });
}

fn broadcast<'a>(out: &mut Vec<Effect>, peers: impl IntoIterator<Item = &'a PublicKey>, message: &Message) {
    for &peer in peers { send(out, peer, message); }
}

fn join_message(call_id: CallId, town: TownId) -> Message {
    Message::CallJoin { call_id, town, muted: false, transcribing: false }
}

fn join_ack_message(call_id: CallId, muted: bool, transcribing: bool, scope: CallScope) -> Message {
    Message::CallJoinAck { call_id, muted, transcribing, seats: scope.ack_seats() }
}

fn answer_message(call_id: CallId, town: TownId) -> Message {
    Message::Answer { call_id, secret: [0; 32], ext: AnswerExt { town: Some(town), media_caps: None } }
}

pub struct CallFlow {
    random: Box<dyn FnMut(&mut [u8]) + Send>,
    me: Option<PublicKey>,
    open_town: Option<TownId>,
    open_peers: BTreeMap<PublicKey, String>,
    town_peers: BTreeMap<TownId, BTreeSet<PublicKey>>,
    town_peer_seats: BTreeMap<TownId, BTreeMap<PublicKey, Id>>,
    peer_names: BTreeMap<PublicKey, String>,
    state: State,
    view: CallView,
    ended: HashSet<CallId>,
    ignored: Ignored,
    answered_transcribe: bool,
    pub(super) origin_us: u64,
}

impl Default for CallFlow {
    fn default() -> Self {
        Self::new(Box::new(|out| makepad_network::tls::os_random(out).expect("call requires OS randomness")))
    }
}

impl CallFlow {
    pub fn new(random: Box<dyn FnMut(&mut [u8]) + Send>) -> Self {
        Self {
            random,
            me: None, open_town: None, open_peers: BTreeMap::new(), peer_names: BTreeMap::new(),
            town_peers: BTreeMap::new(), town_peer_seats: BTreeMap::new(),
            state: State::Idle,
            view: CallView::default(),
            ended: HashSet::new(),
            ignored: Ignored::default(),
            answered_transcribe: false, origin_us: 0,
        }
    }

    pub fn view(&self) -> &CallView { &self.view }
    pub fn ignored(&self) -> Ignored { self.ignored }
    pub fn set_local_key(&mut self, me: PublicKey) { self.me = Some(me); }

    pub fn set_peer_name(&mut self, peer: PublicKey, name: String) {
        self.peer_names.insert(peer, name);
    }

    pub fn set_open_town(&mut self, open: Option<(TownId, Vec<(PublicKey, String)>)>, out: &mut Vec<Effect>) {
        let could_call = self.can_call();
        self.open_town = open.as_ref().map(|(town, _)| *town);
        self.open_peers.clear();
        if let Some((town, peers)) = open {
            for (peer, name) in peers {
                self.set_peer_name(peer, name.clone());
                self.open_peers.insert(peer, name);
            }
            if self.open_peers.is_empty() {
                self.town_peers.remove(&town);
            } else {
                self.town_peers.insert(town, self.open_peers.keys().copied().collect());
            }
        }
        if matches!(self.state, State::Idle) {
            let names = self.open_peers.values().cloned().collect::<Vec<_>>().join(", ");
            if self.view.peer_name != names || self.can_call() != could_call {
                self.view.peer_name = names;
                out.push(Effect::Changed);
            }
        }
    }

    pub fn can_call(&self) -> bool {
        self.open_town.is_some() && !self.open_peers.is_empty()
    }

    pub fn can_join_call(&self, town: TownId, scope: CallScope) -> bool {
        if !matches!(self.state, State::Idle) || self.open_town != Some(town) { return false }
        if let CallScope::Seats { caller, callee } = scope {
            if caller == callee || !self.own_seat(town).is_some_and(|seat| scope.contains_seat(seat)) { return false }
        }
        self.open_peers.keys().any(|peer| self.is_call_eligible(town, scope, peer))
    }

    pub(super) fn town_members(&self, town: TownId) -> Option<&BTreeSet<PublicKey>> { self.town_peers.get(&town) }
    pub(super) fn peer_name(&self, peer: &PublicKey) -> Option<&str> { self.peer_names.get(peer).map(String::as_str) }
    pub(super) fn is_town_member(&self, town: TownId, peer: &PublicKey) -> bool {
        self.town_peers.get(&town).is_some_and(|peers| peers.contains(peer))
    }

    pub fn set_town_peer_seats(&mut self, seats: BTreeMap<TownId, BTreeMap<PublicKey, Id>>) {
        self.town_peer_seats = seats;
    }

    fn peer_seat(&self, town: TownId, peer: &PublicKey) -> Option<Id> {
        self.town_peer_seats.get(&town)?.get(peer).copied()
    }

    fn own_seat(&self, town: TownId) -> Option<Id> {
        self.peer_seat(town, &self.me?)
    }

    fn seat_open_peers(&self, town: TownId, seat: Id) -> impl Iterator<Item = PublicKey> + '_ {
        self.open_peers.keys().copied().filter(move |p|
            self.open_town == Some(town) && self.is_town_member(town, p) && self.peer_seat(town, p) == Some(seat))
    }

    pub fn can_call_seat(&self, town: TownId, seat: Id) -> bool {
        self.own_seat(town).is_some_and(|mine| mine != seat) && self.seat_open_peers(town, seat).next().is_some()
    }

    pub fn calling_peers(&self, town: TownId) -> Vec<PublicKey> {
        match &self.state {
            State::Calling { town: active, rung, .. } if *active == town => rung.iter().copied().collect(),
            _ => Vec::new(),
        }
    }

    fn active_scope(&self) -> Option<(TownId, CallScope)> {
        match self.state {
            State::Idle => None,
            State::Calling { town, scope, .. } | State::Ringing { town, scope, .. }
                | State::Live { town, scope, .. } => Some((town, scope)),
        }
    }

    fn is_call_eligible(&self, town: TownId, scope: CallScope, peer: &PublicKey) -> bool {
        self.is_town_member(town, peer) && match scope {
            CallScope::Town => true,
            CallScope::Seats { caller, callee } => caller != callee
                && self.peer_seat(town, peer).is_some_and(|seat| scope.contains_seat(seat)),
        }
    }

    pub fn set_town_peers(&mut self, towns: BTreeMap<TownId, BTreeSet<PublicKey>>, now: Now, out: &mut Vec<Effect>) {
        self.town_peers = towns;
        let Some((town, scope)) = self.active_scope() else { return };
        let (call_id, tracked): (_, BTreeSet<_>) = match &self.state {
            State::Idle => return,
            State::Calling { call_id, rung, .. } => (*call_id, rung.clone()),
            State::Ringing { call_id, from, roster, joined_peers, .. } =>
                (*call_id, roster.iter().chain(joined_peers.keys()).chain(Some(from)).copied().collect()),
            State::Live { call_id, peers, ringing, .. } =>
                (*call_id, peers.keys().chain(ringing).copied().collect()),
        };
        let cancel_ring = matches!(self.state, State::Calling { joining: false, .. });
        let mut notify = Vec::new();
        let mut changed = false;
        for peer in tracked {
            if self.is_call_eligible(town, scope, &peer) { continue }
            let should_notify = match &self.state {
                State::Calling { rung, .. } => rung.contains(&peer),
                State::Live { peers, ringing, .. } => peers.contains_key(&peer) || ringing.contains(&peer),
                _ => false,
            };
            let why = if self.view.phase == Phase::Live { LeaveWhy::Ended } else { LeaveWhy::Failed };
            if self.departed(peer, why, now, out) {
                if should_notify { notify.push(peer); }
                changed = true;
            }
        }
        if !notify.is_empty() {
            let msg = if cancel_ring { Message::Cancel { call_id } } else { Message::End { call_id } };
            broadcast(out, &notify, &msg);
        }
        if changed { self.changed(out); }
    }

    pub fn session(&self) -> Option<CallId> {
        match self.state {
            State::Idle => None,
            State::Calling { call_id, .. } | State::Ringing { call_id, .. } | State::Live { call_id, .. } => Some(call_id),
        }
    }

    pub fn active_town(&self) -> Option<TownId> {
        match self.state {
            State::Calling { town, .. } | State::Live { town, .. } => Some(town),
            _ => None,
        }
    }

    pub fn ringing_town(&self) -> Option<TownId> {
        match self.state { State::Ringing { town, .. } => Some(town), _ => None }
    }

    pub fn ringing_from(&self) -> Option<PublicKey> {
        match self.state { State::Ringing { from, .. } => Some(from), _ => None }
    }

    pub fn voice_peers(&self) -> impl Iterator<Item = PublicKey> + '_ {
        let peers = match &self.state { State::Live { peers, .. } => Some(peers), _ => None };
        peers.into_iter().flat_map(|p| p.keys().copied())
    }

    pub fn is_voice_peer(&self, peer: &PublicKey) -> bool {
        matches!(&self.state, State::Live { peers, .. } if peers.contains_key(peer))
    }

    pub fn is_peer_muted(&self, peer: &PublicKey) -> bool {
        matches!(&self.state, State::Live { peers, .. } if peers.get(peer).is_some_and(|p| p.muted))
    }

    fn changed(&mut self, out: &mut Vec<Effect>) {
        let name = |key: &PublicKey| self.peer_names.get(key).cloned().unwrap_or_default();
        self.view.town = self.active_town().or_else(|| self.ringing_town());
        self.view.ring_town = self.ringing_town();
        let view = &mut self.view;
        view.participants.clear();
        view.peer_name = match &self.state {
            State::Idle => self.open_peers.values().cloned().collect::<Vec<_>>().join(", "),
            State::Calling { rung, .. } => rung.iter().map(name).collect::<Vec<_>>().join(", "),
            State::Ringing { from, .. } => name(from),
            State::Live { peers, ringing, .. } => {
                view.participants.extend(peers.iter().map(|(&peer, p)| ParticipantShown {
                    peer, name: name(&peer), muted: p.muted, transcribing: p.transcribing,
                }));
                if peers.is_empty() { ringing.iter().map(name).collect::<Vec<_>>().join(", ") }
                else { view.participants.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ") }
            }
        };
        view.peer_muted = !view.participants.is_empty() && view.participants.iter().all(|p| p.muted);
        let transcribing = view.participants.iter().any(|p| p.transcribing);
        if transcribing != view.peer_transcribing {
            view.transcribe_prompt = transcribing && !view.transcribing_me && !self.answered_transcribe;
            view.peer_transcribing = transcribing;
        }
        out.push(Effect::Changed);
    }

    fn secs(&self, now: Now) -> u64 {
        now.mono_us.saturating_sub(self.view.since.expect("active call has a start time")) / 1_000_000
    }

    fn finish(&mut self, now: Now, why: LeaveWhy, out: &mut Vec<Effect>) {
        let Some(session) = self.session() else { return };
        if self.view.transcribing_me { out.push(Effect::Transcribe(false)); }
        match &self.state {
            State::Calling { joining: false, .. } => out.push(Effect::Record(Fact::Leave { session, secs: None, why })),
            State::Ringing { .. } => out.extend([Effect::Ring(false), Effect::Speaker(false)]),
            State::Live { peers, .. } => {
                let secs = (why != LeaveWhy::Failed).then(|| u32::try_from(self.secs(now)).unwrap_or(u32::MAX));
                out.extend(peers.keys().map(|&peer| Effect::PeerVoice { peer, on: false }));
                out.extend([Effect::StopLocalAudio, Effect::Mic(false), Effect::Speaker(false),
                    Effect::Record(Fact::Leave { session, secs, why })]);
            }
            _ => {}
        }
        self.ended.insert(session);
        self.state = State::Idle;
        self.view = CallView::default();
        self.answered_transcribe = false;
    }

    fn enter_live(&mut self, call_id: CallId, town: TownId, scope: CallScope, peers: BTreeMap<PublicKey, ParticipantState>,
        ringing: BTreeSet<PublicKey>, now: Now, out: &mut Vec<Effect>) {
        let was_ringing = self.view.ringing();
        if was_ringing { out.push(Effect::Ring(false)); }
        out.push(Effect::StartLocalAudio);
        out.extend(peers.keys().map(|&peer| Effect::PeerVoice { peer, on: true }));
        if !was_ringing { out.push(Effect::Speaker(true)); }
        out.push(Effect::Mic(true));
        self.state = State::Live { call_id, town, scope, peers, ringing };
        self.view.phase = Phase::Live;
        self.view.since = Some(now.mono_us);
    }

    fn dial(&mut self, call_id: CallId, town: TownId, joining: bool, scope: CallScope,
        rung: BTreeSet<PublicKey>, now: Now, out: &mut Vec<Effect>) {
        let message = if joining {
            self.ended.remove(&call_id);
            join_message(call_id, town)
        } else {
            let target = scope.target_seat().map_or(CallTarget::Town, CallTarget::Seat);
            out.push(Effect::Record(Fact::Open { session: call_id, town, video: false, target }));
            Message::Ring { call_id, secret: [0; 32], video: false, ext: RingExt {
                town: Some(town), roster: rung.iter().copied().collect(), media_caps: None, target_seat: scope.target_seat(),
            } }
        };
        broadcast(out, &rung, &message);
        self.state = State::Calling { call_id, town, joining, scope, rung };
        self.view.phase = Phase::Calling;
        self.view.since = Some(now.mono_us);
        self.origin_us = now.mono_us;
    }

    fn cancel(&mut self, now: Now, out: &mut Vec<Effect>) {
        let State::Calling { call_id, joining, rung, .. } = &self.state else { return };
        let call_id = *call_id;
        let message = if *joining { Message::End { call_id } } else { Message::Cancel { call_id } };
        broadcast(out, rung, &message);
        self.finish(now, LeaveWhy::Ended, out);
    }

    pub fn press(&mut self, press: Press, now: Now, out: &mut Vec<Effect>) {
        match (&mut self.state, press) {
            (State::Idle, Press::Call | Press::CallSeat(_)) => {
                let Some(town) = self.open_town else { return };
                let (scope, rung): (_, BTreeSet<_>) = if let Press::CallSeat(callee) = press {
                    let Some(caller) = self.own_seat(town).filter(|s| *s != callee) else { return };
                    (CallScope::Seats { caller, callee }, self.seat_open_peers(town, callee).collect())
                } else {
                    (CallScope::Town, self.open_peers.keys().copied().collect())
                };
                if rung.is_empty() { return }
                let mut id = [0; 16];
                loop { (self.random)(&mut id); if !self.ended.contains(&id) { break } }
                self.dial(id, town, false, scope, rung, now, out);
            }
            (State::Idle, Press::JoinCall(id, town, scope)) if self.open_town == Some(town) => {
                if let CallScope::Seats { caller, callee } = scope {
                    if caller == callee || !self.own_seat(town).is_some_and(|seat| scope.contains_seat(seat)) { return }
                }
                let rung: BTreeSet<_> = self.open_peers.keys().copied().filter(|p| self.is_call_eligible(town, scope, p)).collect();
                if rung.is_empty() { return }
                self.dial(id, town, true, scope, rung, now, out);
            }
            (State::Ringing { call_id, town, scope, from, from_is_caller, joined_peers, roster }, Press::Answer) => {
                let (call_id, town, scope, mut from, mut from_is_caller) = (*call_id, *town, *scope, *from, *from_is_caller);
                let mut peers = std::mem::take(joined_peers);
                let mut ringing = std::mem::take(roster);
                peers.retain(|p, _| self.is_call_eligible(town, scope, p));
                if !self.is_call_eligible(town, scope, &from) || (!from_is_caller && !peers.contains_key(&from)) {
                    let Some(&next) = peers.keys().next() else {
                        self.finish(now, LeaveWhy::Failed, out);
                        self.changed(out);
                        return;
                    };
                    from = next;
                    from_is_caller = false;
                }
                ringing.retain(|p| self.is_call_eligible(town, scope, p) && !peers.contains_key(p) && *p != from);
                let mut wire = Vec::new();
                if from_is_caller { send(&mut wire, from, &answer_message(call_id, town)); }
                broadcast(&mut wire, peers.keys(), &join_ack_message(call_id, false, false, scope));
                broadcast(&mut wire, &ringing, &join_message(call_id, town));
                peers.entry(from).or_insert_with(|| ParticipantState { joined: true, ..Default::default() });
                self.enter_live(call_id, town, scope, peers, ringing, now, out);
                out.extend(wire);
                out.push(Effect::Record(Fact::Join { session: call_id, from, town }));
            }
            (State::Ringing { call_id, town, from, joined_peers, .. }, Press::Decline | Press::HangUp) => {
                let peers: BTreeSet<_> = joined_peers.keys().copied().chain(Some(*from)).collect();
                broadcast(out, &peers, &Message::Decline { call_id: *call_id, why: Why::Declined });
                out.push(Effect::Record(Fact::Decline { session: *call_id, why: DeclineWhy::Declined, from: *from, town: *town }));
                self.finish(now, LeaveWhy::Ended, out);
            }
            (State::Calling { .. }, Press::Cancel | Press::HangUp) => self.cancel(now, out),
            (State::Live { call_id, peers, ringing, .. }, Press::HangUp) => {
                let message = Message::End { call_id: *call_id };
                let targets: Vec<_> = peers.keys().chain(ringing.iter()).copied().collect();
                self.finish(now, LeaveWhy::Ended, out);
                broadcast(out, &targets, &message);
            }
            (State::Live { call_id, peers, .. }, Press::Transcribe | Press::StopTranscribing) => {
                let on = press == Press::Transcribe;
                if on == self.view.transcribing_me { return }
                self.view.transcribing_me = on;
                self.answered_transcribe = true;
                self.view.transcribe_prompt = false;
                out.push(Effect::Transcribe(on));
                broadcast(out, peers.keys(), &Message::Transcribe { call_id: *call_id, on });
            }
            (State::Live { .. }, Press::NotMe) if self.view.transcribe_prompt => {
                self.answered_transcribe = true;
                self.view.transcribe_prompt = false;
            }
            (State::Live { call_id, peers, .. }, Press::Mute | Press::Unmute) => {
                let on = press == Press::Mute;
                if self.view.muted == on { return }
                self.view.muted = on;
                self.view.unmuting = !on;
                out.push(Effect::Mic(!on));
                broadcast(out, peers.keys(), &Message::Mute { call_id: *call_id, on });
            }
            _ => return,
        }
        self.changed(out);
    }

    fn incoming_ring(&mut self, sender: PublicKey, id: CallId, ext: RingExt, now: Now, out: &mut Vec<Effect>) {
        let Some(town) = ext.town else { self.ignored.refused += 1; return };
        if !self.is_town_member(town, &sender) { self.ignored.refused += 1; return }
        let scope = if let Some(callee) = ext.target_seat {
            let Some(caller) = self.peer_seat(town, &sender).filter(|s| *s != callee && self.own_seat(town) == Some(callee)) else {
                self.ignored.refused += 1;
                return;
            };
            CallScope::Seats { caller, callee }
        } else { CallScope::Town };
        let roster = || ext.roster.iter().copied()
            .filter(|p| Some(*p) != self.me && *p != sender && self.is_call_eligible(town, scope, p)).collect::<BTreeSet<_>>();
        match &self.state {
            State::Idle => {
                self.state = State::Ringing { call_id: id, town, scope, from: sender, from_is_caller: true,
                    roster: roster(), joined_peers: BTreeMap::new() };
                self.origin_us = now.mono_us;
                self.view.since = Some(now.mono_us);
                self.view.phase = Phase::Ringing;
                out.extend([Effect::Speaker(true), Effect::Ring(true)]);
            }
            State::Calling { call_id, town: ours, joining: false, rung, scope: ours_scope }
                if town == *ours && rung.contains(&sender) && ours_scope.same_seats(scope) => {
                if self.me.is_none_or(|me| me <= sender) {
                    if id != *call_id { self.ended.insert(id); }
                    return;
                }
                let ringing = roster();
                if id != *call_id {
                    broadcast(out, rung.iter().filter(|p| **p != sender), &Message::End { call_id: *call_id });
                    out.push(Effect::Record(Fact::Leave { session: *call_id, secs: None, why: LeaveWhy::Ended }));
                    self.ended.insert(*call_id);
                }
                self.origin_us = now.mono_us;
                let peers = BTreeMap::from([(sender, ParticipantState { joined: true, ..Default::default() })]);
                let mut wire = Vec::new();
                send(&mut wire, sender, &answer_message(id, town));
                broadcast(&mut wire, &ringing, &join_message(id, town));
                self.enter_live(id, town, scope, peers, ringing, now, out);
                out.extend(wire);
                out.push(Effect::Record(Fact::Join { session: id, from: sender, town }));
            }
            _ if self.session() == Some(id) => { self.ignored.stale += 1; return }
            _ => {
                self.ended.insert(id);
                out.push(Effect::Record(Fact::Decline { session: id, why: DeclineWhy::Busy, from: sender, town }));
                send(out, sender, &Message::Decline { call_id: id, why: Why::Busy });
            }
        }
        self.changed(out);
    }

    fn departed(&mut self, sender: PublicKey, why: LeaveWhy, now: Now, out: &mut Vec<Effect>) -> bool {
        match &mut self.state {
            State::Idle => return false,
            State::Calling { rung, .. } => {
                if !rung.remove(&sender) { return false }
                if rung.is_empty() { self.finish(now, why, out); }
            }
            State::Ringing { from, from_is_caller, roster, joined_peers, .. } => {
                let removed = roster.remove(&sender) | joined_peers.remove(&sender).is_some();
                if *from == sender {
                    if let Some(&next) = joined_peers.keys().next() {
                        *from = next;
                        *from_is_caller = false;
                    } else {
                        self.finish(now, why, out);
                    }
                } else if !removed { return false }
            }
            State::Live { call_id, peers, ringing, .. } => {
                if peers.contains_key(&sender) {
                    ringing.remove(&sender);
                    if peers.len() == 1 && (ringing.is_empty() || now.mono_us.saturating_sub(self.origin_us) >= RING_WAIT) {
                        let end = Message::End { call_id: *call_id };
                        let waiting = std::mem::take(ringing);
                        self.finish(now, why, out);
                        broadcast(out, &waiting, &end);
                    } else {
                        peers.remove(&sender);
                        out.push(Effect::PeerVoice { peer: sender, on: false });
                    }
                } else if ringing.remove(&sender) {
                    if peers.is_empty() && ringing.is_empty() {
                        self.finish(now, if why == LeaveWhy::Dropped { why } else { LeaveWhy::Ended }, out);
                    }
                } else { return false }
            }
        }
        true
    }

    pub fn peer_down(&mut self, peer: PublicKey, now: Now, out: &mut Vec<Effect>) {
        self.peer_names.remove(&peer);
        self.town_peers.retain(|_, peers| {
            peers.remove(&peer);
            !peers.is_empty()
        });
        for seats in self.town_peer_seats.values_mut() { seats.remove(&peer); }
        let target_changed = self.open_peers.remove(&peer).is_some();
        let why = if self.view.phase == Phase::Live { LeaveWhy::Dropped } else { LeaveWhy::Failed };
        if self.departed(peer, why, now, out) || target_changed { self.changed(out); }
    }

    pub fn receive(&mut self, sender: PublicKey, body: &[u8], now: Now, out: &mut Vec<Effect>) {
        let message = match decode(body) {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(_) => { self.ignored.refused += 1; return }
        };
        let Some(id) = message.call_id() else { return };
        if matches!(message, Message::View { .. } | Message::Share { .. } | Message::ShareOff { .. }
            | Message::RecordingState { .. } | Message::RecordingConsent { .. }) { return }
        if self.ended.contains(&id) { self.ignored.stale += 1; return }
        if let Message::Ring { ext, .. } = message {
            self.incoming_ring(sender, id, ext, now, out);
            return;
        }
        if self.session() != Some(id) { self.ignored.stale += 1; return }
        if matches!(message, Message::End { .. } | Message::Cancel { .. } | Message::Decline { .. }) {
            let live = self.view.phase == Phase::Live;
            let why = if live == matches!(message, Message::End { .. }) { LeaveWhy::Ended } else { LeaveWhy::Failed };
            if self.departed(sender, why, now, out) { self.changed(out); }
            else { self.ignored.stale += 1; }
            return;
        }
        if matches!(message, Message::Answer { .. } | Message::CallJoin { .. } | Message::CallJoinAck { .. }) {
            let eligible = self.active_scope().is_some_and(|(town, scope)| self.is_call_eligible(town, scope, &sender));
            if !eligible { self.ignored.stale += 1; return }
        }
        let state = match &message {
            Message::CallJoin { muted, transcribing, .. } | Message::CallJoinAck { muted, transcribing, .. } =>
                ParticipantState { muted: *muted, transcribing: *transcribing, joined: true },
            _ => ParticipantState { joined: true, ..Default::default() },
        };
        match (&mut self.state, &message) {
            (State::Calling { town, joining, scope, rung, .. }, Message::CallJoin { .. } | Message::Answer { .. } | Message::CallJoinAck { .. }) => {
                let (town, joining, scope) = (*town, *joining, *scope);
                match message {
                    Message::CallJoin { town: t, .. } if t == town && joining => {
                        if rung.insert(sender) { send(out, sender, &join_message(id, town)); }
                        send(out, sender, &join_ack_message(id, false, false, scope));
                        return;
                    }
                    Message::CallJoin { town: t, .. } if t == town => {}
                    Message::Answer { ref ext, .. } if !joining && rung.contains(&sender) && ext.town == Some(town) => {}
                    Message::CallJoinAck { seats, .. } if joining && ack_scope_matches(scope, seats) => {}
                    _ => { self.ignored.stale += 1; return }
                }
                rung.remove(&sender);
                let ringing = if joining { BTreeSet::new() } else { std::mem::take(rung) };
                self.enter_live(id, town, scope, BTreeMap::from([(sender, state)]), ringing, now, out);
                if joining { out.push(Effect::Record(Fact::Join { session: id, from: sender, town })); }
                if matches!(message, Message::CallJoin { .. }) {
                    send(out, sender, &join_ack_message(id, false, false, scope));
                }
            }
            (State::Ringing { town, roster, joined_peers, .. }, Message::CallJoin { town: t, .. }) if town == t => {
                roster.remove(&sender);
                joined_peers.insert(sender, state);
            }
            (State::Live { town, scope, peers, ringing, .. }, Message::CallJoin { .. } | Message::Answer { .. } | Message::CallJoinAck { .. }) => {
                match message {
                    Message::Answer { ref ext, .. } if ext.town == Some(*town) && ringing.contains(&sender) => {}
                    Message::CallJoin { town: t, .. } if t == *town => {}
                    Message::CallJoinAck { seats, .. } if ack_scope_matches(*scope, seats) => {}
                    _ => { self.ignored.stale += 1; return }
                }
                ringing.remove(&sender);
                let added = peers.insert(sender, state).is_none();
                if added { out.push(Effect::PeerVoice { peer: sender, on: true }); }
                if matches!(message, Message::CallJoin { .. }) {
                    send(out, sender, &join_ack_message(id, self.view.muted, self.view.transcribing_me, *scope));
                } else if added {
                    if self.view.muted { send(out, sender, &Message::Mute { call_id: id, on: true }); }
                    if self.view.transcribing_me { send(out, sender, &Message::Transcribe { call_id: id, on: true }); }
                }
            }
            (State::Live { peers, .. }, Message::Mute { on, .. } | Message::Transcribe { on, .. }) => {
                let Some(p) = peers.get_mut(&sender) else { self.ignored.stale += 1; return };
                let flag = if matches!(message, Message::Mute { .. }) { &mut p.muted } else { &mut p.transcribing };
                if *flag == *on { return }
                *flag = *on;
            }
            _ => { self.ignored.stale += 1; return }
        }
        self.changed(out);
    }

    pub fn tick(&mut self, now: Now, out: &mut Vec<Effect>) {
        let expired = now.mono_us.saturating_sub(self.origin_us) >= RING_WAIT;
        match &mut self.state {
            State::Calling { .. } if expired => self.cancel(now, out),
            State::Ringing { .. } if expired => self.finish(now, LeaveWhy::Ended, out),
            State::Live { call_id, peers, ringing, .. } if peers.is_empty() && expired => {
                let message = Message::End { call_id: *call_id };
                let waiting = std::mem::take(ringing);
                self.finish(now, LeaveWhy::Ended, out);
                broadcast(out, &waiting, &message);
            }
            State::Live { .. } => {
                let secs = self.secs(now);
                if self.view.duration_secs == secs { return }
                self.view.duration_secs = secs;
            }
            _ => return,
        }
        self.changed(out);
    }
    pub fn mic_flowing(&mut self, _now: Now, out: &mut Vec<Effect>) {
        if self.view.unmuting {
            self.view.unmuting = false;
            out.push(Effect::Changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use Effect::{Changed, Mic, Ring, Speaker, StartLocalAudio, StopLocalAudio};
    use Phase::{Calling, Idle, Live, Ringing};

    const TOWN: TownId = [0xAA; 32];
    fn key(side: usize) -> PublicKey { [side as u8; 65] }
    fn voice(side: usize, on: bool) -> Effect { Effect::PeerVoice { peer: key(side), on } }
    const NAMES: [&str; 2] = ["East", "West"];
    fn leave(byte: u8, secs: Option<u32>, why: LeaveWhy) -> Fact { Fact::Leave { session: id(byte), secs, why } }
    fn decline(byte: u8, why: DeclineWhy, from: usize) -> Fact { Fact::Decline { session: id(byte), why, from: key(from), town: TOWN } }
    fn id(byte: u8) -> CallId {
        [byte; 16]
    }
    fn send(to: usize, msg: Message) -> Effect {
        Effect::SendTo { peer: key(to), body: msg.encode() }
    }

    struct Side {
        ctl: CallFlow,
        wire: VecDeque<Vec<u8>>,
        facts: Vec<Fact>,
        mic: bool,
        speaker: bool,
        ring: bool,
        media: bool,
    }

    impl Side {
        fn new(mut seed: u8) -> Self {
            let ctl = CallFlow::new(Box::new(move |bytes| {
                bytes.fill(seed);
                seed = seed.wrapping_add(1);
            }));
            Self { ctl, wire: VecDeque::new(), facts: Vec::new(), mic: false, speaker: false, ring: false, media: false }
        }
    }

    struct Pair {
        sides: [Side; 2],
        now: Now,
    }

    impl Pair {
        fn new() -> Self {
            Self::with_seeds(10, 80)
        }

        fn with_seeds(a: u8, b: u8) -> Self {
            let mut p = Self { sides: [Side::new(a), Side::new(b)], now: Now { mono_us: 500_000 } };
            for i in 0..2 {
                p.sides[i].ctl.set_local_key(key(i));
                p.link(i);
            }
            p
        }

        fn link(&mut self, i: usize) {
            let peer = 1 - i;
            self.input(i, &[Changed], Idle, |ctl, _, fx| {
                ctl.set_open_town(Some((TOWN, vec![(key(peer), NAMES[peer].into())])), fx)
            });
            assert_eq!(self.sides[i].ctl.view().peer_name, NAMES[peer]);
        }

        fn input(
            &mut self,
            i: usize,
            expected: &[Effect],
            phase: Phase,
            f: impl FnOnce(&mut CallFlow, Now, &mut Vec<Effect>),
        ) {
            let side = &mut self.sides[i];
            let mut effects = Vec::new();
            f(&mut side.ctl, self.now, &mut effects);
            effects.retain(|effect| {
                if let Effect::Record(fact) = effect { side.facts.push(fact.clone()); false } else { true }
            });
            assert_eq!(effects, expected, "ordered effects on side {i}");
            for effect in effects {
                match effect {
                    Effect::SendTo { peer, body } => {
                        assert_eq!(peer, key(1-i), "pair signal addresses the other seat");
                        side.wire.push_back(body);
                    }
                    Effect::PeerVoice { peer, .. } => assert_eq!(peer, key(1-i), "voice gate addresses the other seat"),
                    StartLocalAudio => {
                        assert!(!side.media);
                        side.media = true;
                    }
                    StopLocalAudio => {
                        assert!(side.media);
                        side.media = false;
                    }
                    Mic(on) => {
                        assert!(!on || side.media);
                        side.mic = on;
                    }
                    Speaker(on) => side.speaker = on,
                    Ring(on) => {
                        assert!(!on || side.speaker);
                        side.ring = on;
                    }
                    Effect::Record(_) => unreachable!(),
                    Effect::Transcribe(_) => {},
                    Changed => {}
                }
            }
            let view = side.ctl.view();
            assert_eq!(view.phase, phase, "phase on side {i}");
            assert_eq!(view.ringing(), phase == Ringing);
            assert_eq!(side.ring, phase == Ringing);
            assert_eq!(side.media, phase == Live);
            assert_eq!(side.speaker, matches!(phase, Ringing | Live));
            assert_eq!(side.mic, phase == Live && !view.muted);
        }

        fn advance(&mut self, us: u64) {
            self.now.mono_us += us;
        }
        fn press(&mut self, i: usize, p: Press, fx: &[Effect], phase: Phase) {
            self.input(i, fx, phase, |c, n, out| c.press(p, n, out));
        }
        fn receive(&mut self, i: usize, body: &[u8], fx: &[Effect], phase: Phase) {
            self.input(i, fx, phase, |c, n, out| c.receive(key(1-i), body, n, out));
        }
        fn deliver(&mut self, from: usize, fx: &[Effect], phase: Phase) {
            let body = self.sides[from].wire.pop_front().expect("a message is pending");
            self.receive(1 - from, &body, fx, phase);
        }
        fn tick(&mut self, i: usize, fx: &[Effect], phase: Phase) {
            self.input(i, fx, phase, CallFlow::tick);
        }
        fn unlink(&mut self, i: usize, fx: &[Effect]) {
            self.input(i, fx, Idle, |c, n, out| c.peer_down(key(1-i), n, out));
            assert!(self.sides[i].ctl.voice_peers().next().is_none());
        }
        fn flowing(&mut self, i: usize, fx: &[Effect]) {
            self.input(i, fx, Live, CallFlow::mic_flowing);
        }
        fn dial(&mut self, i: usize, byte: u8) {
            self.press(
                i,
                Press::Call,
                &[send(1-i, Message::Ring { call_id: id(byte), secret: [0; 32], video: false, ext: RingExt { town: Some(TOWN), roster: vec![key(1-i)], media_caps: None, target_seat: None } }), Changed],
                Calling,
            );
            assert_eq!(self.sides[i].facts.last(), Some(&Fact::Open { session: id(byte), video: false, town: TOWN, target: CallTarget::Town }), "caller records its own ring");
        }
        fn ringing(&mut self) {
            self.dial(0, 10);
            self.deliver(0, &[Speaker(true), Ring(true), Changed], Ringing);
            assert!(self.sides[1].facts.is_empty(), "ringing alone records nothing");
        }
        fn answer_press(&mut self) {
            self.press(
                1,
                Press::Answer,
                &[
                    Ring(false),
                    StartLocalAudio, voice(0, true),
                    Mic(true),
                    send(0, answer_message(id(10), TOWN)),
                    Changed,
                ],
                Live,
            );
        }
        fn answer(&mut self) {
            self.answer_press();
            self.deliver(1, &[StartLocalAudio, voice(1, true), Speaker(true), Mic(true), Changed], Live);
            assert_eq!(self.sides[1].facts, [Fact::Join { session: id(10), from: key(0), town: TOWN }], "answer signs the callee’s join");
        }
        fn hang(&mut self, i: usize, byte: u8) {
            self.press(
                i,
                Press::HangUp,
                &[voice(1-i, false), StopLocalAudio, Mic(false), Speaker(false), send(1-i, Message::End { call_id: id(byte) }), Changed],
                Idle,
            );
        }
        fn hanging_up(&mut self, i: usize, byte: u8) {
            self.hang(i, byte);
            self.deliver(i, &[voice(i, false), StopLocalAudio, Mic(false), Speaker(false), Changed], Idle);
        }

        fn closed(&self) {
            for s in &self.sides {
                assert_eq!(s.ctl.view().phase, Idle);
                assert_eq!(s.ctl.view().since, None);
                assert!(!s.media && !s.mic && !s.speaker && !s.ring);
                assert!(!s.ctl.view().muted && !s.ctl.view().unmuting && !s.ctl.view().peer_muted);
                assert!(s.wire.is_empty(), "all messages delivered");
            }
        }
    }

    #[test]
    fn ring_answer_and_either_person_hangs_up() {
        for who in 0..2 {
            let mut p = Pair::new();
            p.ringing();
            p.advance(6_000_000);
            p.answer();
            assert_eq!(p.sides[0].ctl.view().since, Some(p.now.mono_us));
            assert_eq!(p.sides[1].ctl.view().since, Some(p.now.mono_us));
            p.advance(2_950_000);
            for i in 0..2 {
                p.tick(i, &[Changed], Live);
                assert_eq!(p.sides[i].ctl.view().duration_secs, 2);
                p.tick(i, &[], Live);
            }
            p.hanging_up(who, 10);
            for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(10, Some(2), LeaveWhy::Ended)), "each participant records its duration"); }
            p.closed();
        }
    }

    #[test]
    fn callee_declines_or_hangs_up_while_ringing() {
        for press in [Press::Decline, Press::HangUp] {
            let mut p = Pair::new();
            p.ringing();
            p.press(
                1,
                press,
                &[send(0, Message::Decline { call_id: id(10), why: Why::Declined }), Ring(false), Speaker(false), Changed],
                Idle,
            );
            p.deliver(1, &[Changed], Idle);
            assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller closes the ring");
            assert_eq!(p.sides[1].facts, [decline(10, DeclineWhy::Declined, 0)], "callee signs the decline");
            p.closed();
        }
    }

    #[test]
    fn caller_cancels_and_peer_misses_the_call() {
        let mut p = Pair::new();
        p.ringing();
        p.press(0, Press::Cancel, &[send(1, Message::Cancel { call_id: id(10) }), Changed], Idle);
        p.deliver(0, &[Ring(false), Speaker(false), Changed], Idle);
        assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller ends the unanswered ring");
        assert!(p.sides[1].facts.is_empty(), "the missed ring is derived");
        p.closed();
    }

    #[test]
    fn ring_timeout_at_thirty_seconds_on_either_peer() {
        for callee_ticks_first in [false, true] {
            let mut p = Pair::new();
            p.ringing();
            p.advance(29_999_999);
            p.tick(0, &[], Calling);
            p.tick(1, &[], Ringing);
            p.advance(1);
            if callee_ticks_first {
                p.tick(1, &[Ring(false), Speaker(false), Changed], Idle);
            }
            p.tick(0, &[send(1, Message::Cancel { call_id: id(10) }), Changed], Idle);
            if callee_ticks_first {
                p.deliver(0, &[], Idle);
                assert_eq!(p.sides[1].ctl.ignored().stale, 1);
            } else {
                p.deliver(0, &[Ring(false), Speaker(false), Changed], Idle);
            }
            assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller ends the unanswered ring");
            assert!(p.sides[1].facts.is_empty(), "the missed ring is derived");
            p.closed();
        }
    }

    #[test]
    fn glare_keeps_the_lower_id_in_both_orders() {
        for (winning, losing) in [(10, 80), (80, 10)] {
            for first in 0..2 {
                let mut p = Pair::with_seeds(winning, losing);
                p.dial(0, winning);
                p.dial(1, losing);
                for from in [first, 1 - first] {
                    if from == 0 {
                        p.deliver(0, &[StartLocalAudio, voice(0, true), Speaker(true), Mic(true),
                            send(0, answer_message(id(winning), TOWN)), Changed], Live);
                    } else {
                        p.deliver(1, &[], Calling);
                    }
                }
                p.deliver(1, &[StartLocalAudio, voice(1, true), Speaker(true), Mic(true), Changed], Live);
                assert_eq!(&p.sides[1].facts[1..], &[leave(losing, None, LeaveWhy::Ended),
                    Fact::Join { session: id(winning), from: key(0), town: TOWN }], "crossed caller ends its own ring and joins the winning session");
                p.hanging_up(1, winning);
                for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(winning, Some(0), LeaveWhy::Ended)), "each participant records its duration"); }
                p.closed();
            }
        }
    }

    #[test]
    fn busy_and_active_facts_keep_their_own_session_ids() {
        let mut p = Pair::new();
        p.ringing();
        p.answer();
        // The peer re-links and calls again before our side learns it lost the original call.
        p.unlink(1, &[voice(0, false), StopLocalAudio, Mic(false), Speaker(false), Changed]);
        p.link(1);
        p.advance(2_000_000);
        p.dial(1, 80);
        p.deliver(1, &[send(1, Message::Decline { call_id: id(80), why: Why::Busy }), Changed], Live);
        p.deliver(0, &[Changed], Idle);
        p.hang(0, 10);
        p.deliver(0, &[], Idle);
        assert_eq!(&p.sides[0].facts[1..], &[decline(80, DeclineWhy::Busy, 1), leave(10, Some(2), LeaveWhy::Ended)], "busy refers to the second ring only");
        assert_eq!(p.sides[1].facts[1], leave(10, Some(0), LeaveWhy::Dropped), "link loss ends the first call");
        assert_eq!(p.sides[1].facts.last(), Some(&leave(80, None, LeaveWhy::Ended)), "the second caller closes its ring");
        p.closed();
    }

    #[test]
    fn unlink_closes_pending_and_live_calls() {
        for live in [false, true] {
            let mut p = Pair::new();
            p.ringing();
            if live {
                p.answer();
            }
            p.advance(3_500_000);
            if live {
                for i in 0..2 {
                    p.unlink(i, &[voice(1-i, false), StopLocalAudio, Mic(false), Speaker(false), Changed]);
                }
                for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(10, Some(3), LeaveWhy::Dropped)), "disconnect records elapsed time"); }
            } else {
                p.unlink(0, &[Changed]);
                p.unlink(1, &[Ring(false), Speaker(false), Changed]);
                assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Failed)), "caller records link failure");
                assert!(p.sides[1].facts.is_empty(), "unanswered callee stays silent");
            }
            p.press(0, Press::Call, &[], Idle);
            p.closed();
        }
    }

    #[test]
    fn transcription_is_each_speakers_choice_for_one_live_session() {
        let mut p = Pair::new();
        let msg = |on| Message::Transcribe { call_id: id(10), on };
        let local = |to, on| [Effect::Transcribe(on), send(to, msg(on)), Changed];
        p.press(0, Press::Transcribe, &[], Idle);
        p.ringing();
        p.press(0, Press::Transcribe, &[], Calling);
        p.receive(1, &msg(true).encode(), &[], Ringing);
        p.press(1, Press::Transcribe, &[], Ringing);
        let origin = p.now.mono_us;
        p.advance(300_000);
        p.answer();
        for s in &p.sides { assert_eq!(s.ctl.origin_us, origin, "answer preserves the ring clock"); }
        p.receive(1, &Message::Transcribe { call_id: id(99), on: true }.encode(), &[], Live);
        p.press(0, Press::Transcribe, &local(1, true), Live);
        p.deliver(0, &[Changed], Live);
        let b = p.sides[1].ctl.view();
        assert!(b.peer_transcribing && b.transcribe_prompt && !b.transcribing_me, "peer announcement only invites");
        p.press(1, Press::NotMe, &[Changed], Live);
        p.press(0, Press::StopTranscribing, &local(1, false), Live);
        p.deliver(0, &[Changed], Live);
        assert!(!p.sides[1].ctl.view().peer_transcribing);
        p.press(0, Press::Transcribe, &local(1, true), Live);
        p.deliver(0, &[Changed], Live);
        assert!(!p.sides[1].ctl.view().transcribe_prompt, "Not me lasts through a peer restart");
        p.press(1, Press::Transcribe, &local(0, true), Live);
        p.deliver(1, &[Changed], Live);
        for s in &p.sides {
            let v = s.ctl.view();
            assert!(v.transcribing_me && v.peer_transcribing && !v.transcribe_prompt);
        }
        for i in 0..2 {
            p.unlink(i, &[Effect::Transcribe(false), voice(1-i, false), StopLocalAudio, Mic(false), Speaker(false), Changed]);
            let v = p.sides[i].ctl.view();
            assert!(!v.transcribing_me && !v.peer_transcribing && !v.transcribe_prompt, "end clears consent and invitation");
        }
    }

    #[test]
    fn mute_and_unmute_wait_for_microphone_progress() {
        let mut p = Pair::new();
        p.ringing();
        p.answer();
        for i in 0..2 {
            for on in [true, false] {
                let press = if on { Press::Mute } else { Press::Unmute };
                p.press(i, press, &[Mic(!on), send(1-i, Message::Mute { call_id: id(10), on }), Changed], Live);
                p.press(i, press, &[], Live);
                assert_eq!(p.sides[i].ctl.view().unmuting, !on);
                p.deliver(i, &[Changed], Live);
                assert_eq!(p.sides[1 - i].ctl.view().peer_muted, on);
            }
            p.advance(90_000_000);
            p.tick(i, &[Changed], Live);
            assert!(p.sides[i].ctl.view().unmuting);
            p.flowing(i, &[Changed]);
            assert!(!p.sides[i].ctl.view().unmuting);
            p.flowing(i, &[]);
        }
        p.hanging_up(0, 10);
        for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(10, Some(180), LeaveWhy::Ended)), "each participant records its duration"); }
        p.closed();
    }

    #[test]
    fn stale_and_refused_messages_never_reply_or_reopen() {
        let mut p = Pair::new();
        p.ringing();
        p.receive(0, &[2, 1], &[], Calling);
        p.receive(1, &[1, 2, 3], &[], Ringing);
        p.receive(0, &Message::Answer { call_id: id(42), secret: [0; 32], ext: AnswerExt { town: Some(TOWN), ..Default::default() } }.encode(), &[], Calling);
        p.receive(1, &Message::End { call_id: id(42) }.encode(), &[], Ringing);
        p.answer();
        p.hanging_up(0, 10);
        for i in 0..2 {
            p.receive(i, &Message::Ring { call_id: id(10), secret: [0; 32], video: false, ext: RingExt { town: Some(TOWN), ..Default::default() } }.encode(), &[], Idle);
            p.receive(i, &[1, 99, 0xff], &[], Idle);
            p.receive(i, &Message::CallJoinAck { call_id: id(10), muted: false, transcribing: true, seats: None }.encode(), &[], Idle);
            assert_eq!(p.sides[i].ctl.ignored(), Ignored { stale: 3, refused: 1 });
            p.receive(i, &Message::Ring { call_id: id(44), secret: [0; 32], video: false, ext: RingExt::default() }.encode(), &[], Idle);
            assert_eq!(p.sides[i].ctl.ignored().refused, 2, "a fresh ring without a town is refused");
        }
        for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(10, Some(0), LeaveWhy::Ended)), "each participant records its duration"); }
        p.closed();
    }

    #[test]
    fn matching_endings_close_even_outside_the_usual_phase() {
        for terminal in [
            Message::Cancel { call_id: id(10) },
            Message::Decline { call_id: id(10), why: Why::Declined },
            Message::End { call_id: id(10) },
        ] {
            for live in [false, true] {
                let mut p = Pair::new();
                p.ringing();
                if live {
                    p.answer();
                }
                let body = terminal.encode();
                for i in 0..2 {
                    let expected: &[Effect] = if live {
                        &[voice(1-i, false), StopLocalAudio, Mic(false), Speaker(false), Changed]
                    } else if i == 1 {
                        &[Ring(false), Speaker(false), Changed]
                    } else {
                        &[Changed]
                    };
                    p.receive(i, &body, expected, Idle);
                    p.receive(i, &body, &[], Idle);
                    assert_eq!(p.sides[i].ctl.ignored().stale, 1);
                }
                let why = match (live, &terminal) {
                    (true, Message::End { .. }) | (false, Message::Cancel { .. } | Message::Decline { .. }) => LeaveWhy::Ended,
                    _ => LeaveWhy::Failed,
                };
                let fact = leave(10, (live && why == LeaveWhy::Ended).then_some(0), why);
                assert_eq!(p.sides[0].facts.last(), Some(&fact), "the caller records how its call ended");
                assert_eq!(p.sides[1].facts.last(), live.then_some(&fact), "only an answering callee records a leave");
                p.closed();
            }
        }
    }

    fn cross_answer_and_cancel(timeout: bool) {
        let mut p = Pair::new();
        p.ringing();
        if timeout {
            p.advance(RING_WAIT);
        }
        p.answer_press();
        let cancel = [send(1, Message::Cancel { call_id: id(10) }), Changed];
        if timeout {
            p.tick(0, &cancel, Idle);
        } else {
            p.press(0, Press::Cancel, &cancel, Idle);
        }
        // Both sends are still on the wire. Only the answerer opened devices.
        p.deliver(0, &[voice(0, false), StopLocalAudio, Mic(false), Speaker(false), Changed], Idle);
        p.deliver(1, &[], Idle);
        assert_eq!(p.sides[0].ctl.ignored().stale, 1);
        assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller cancels");
        assert_eq!(p.sides[1].facts.last(), Some(&leave(10, None, LeaveWhy::Failed)), "crossed answer failed");
        p.closed();
    }

    #[test]
    fn answer_crosses_cancel() {
        cross_answer_and_cancel(false);
    }
    #[test]
    fn answer_crosses_caller_timeout() {
        cross_answer_and_cancel(true);
    }

    #[test]
    fn simultaneous_hangups_are_stale_on_arrival() {
        let mut p = Pair::new();
        p.ringing();
        p.answer();
        p.advance(1_250_000);
        for i in 0..2 {
            p.hang(i, 10);
        }
        for i in 0..2 {
            p.deliver(i, &[], Idle);
            assert_eq!(p.sides[1 - i].ctl.ignored().stale, 1);
        }
        for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(10, Some(1), LeaveWhy::Ended)), "each participant records its duration"); }
        p.closed();
    }

    #[test]
    fn decline_crosses_cancel() {
        let mut p = Pair::new();
        p.ringing();
        p.press(0, Press::Cancel, &[send(1, Message::Cancel { call_id: id(10) }), Changed], Idle);
        p.press(
            1,
            Press::Decline,
            &[send(0, Message::Decline { call_id: id(10), why: Why::Declined }), Ring(false), Speaker(false), Changed],
            Idle,
        );
        for i in 0..2 {
            p.deliver(i, &[], Idle);
            assert_eq!(p.sides[1 - i].ctl.ignored().stale, 1);
        }
        assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller cancels");
        assert_eq!(p.sides[1].facts, [decline(10, DeclineWhy::Declined, 0)], "callee declined independently");
        p.closed();
    }

    // Devices 1 and 2 belong to Ada, 3 to Ben, and 4 and 5 to Ana.
    fn seat_flow(device: usize, linked: &[usize]) -> CallFlow {
        let mut flow = CallFlow::new(Box::new(|bytes| bytes.fill(10)));
        flow.set_local_key(key(device));
        flow.set_town_peer_seats(BTreeMap::from([(TOWN, BTreeMap::from([
            (key(1), [1; 32]), (key(2), [1; 32]), (key(3), [3; 32]),
            (key(4), [4; 32]), (key(5), [4; 32]),
        ]))]));
        flow.set_open_town(Some((TOWN, linked.iter().map(|&i| (key(i), format!("Peer {i}"))).collect())), &mut Vec::new());
        flow
    }

    fn pressed(flow: &mut CallFlow, press: Press) -> Vec<Effect> {
        let mut out = Vec::new();
        flow.press(press, Now::default(), &mut out);
        out
    }

    fn received(flow: &mut CallFlow, from: usize, message: &Message) -> Vec<Effect> {
        let mut out = Vec::new();
        flow.receive(key(from), &message.encode(), Now::default(), &mut out);
        out
    }

    fn sent(effects: &[Effect]) -> Vec<(PublicKey, Message)> {
        effects.iter().filter_map(|e| match e {
            Effect::SendTo { peer, body } => Some((*peer, decode(body).unwrap().unwrap())),
            _ => None,
        }).collect()
    }

    #[test]
    fn call_seat_rings_only_target_seats_devices_and_isolates_glare_and_late_join() {
        let mut ana = seat_flow(4, &[1, 2, 3, 5]);
        let ada = [1; 32];
        assert!(ana.can_call_seat(TOWN, ada), "both of Ada's devices are reachable");
        for seat in [[4; 32], [8; 32]] {
            assert!(!ana.can_call_seat(TOWN, seat) && pressed(&mut ana, Press::CallSeat(seat)).is_empty(),
                "dialling self or an unknown seat must produce no facts or wire effects");
        }
        let dial = pressed(&mut ana, Press::CallSeat(ada));
        let session = ana.session().unwrap();
        let ring = || Message::Ring { call_id: session, secret: [0; 32], video: false,
            ext: RingExt { town: Some(TOWN), roster: vec![key(1), key(2)], target_seat: Some(ada), ..Default::default() } };
        assert_eq!(sent(&dial), [(key(1), ring()), (key(2), ring())], "only Ada's devices receive the targeted roster");
        assert!(dial.contains(&Effect::Record(Fact::Open { session, town: TOWN, target: CallTarget::Seat(ada), video: false })),
            "the opening fact must carry the intended seat");
        for from in [3, 1] {
            let other = Message::Ring { call_id: id(80 + from as u8), secret: [0; 32], video: false,
                ext: RingExt { town: Some(TOWN), ..Default::default() } };
            let busy = received(&mut ana, from, &other);
            assert!(busy.contains(&Effect::Record(decline(80 + from as u8, DeclineWhy::Busy, from))), "competing call records Busy");
            assert_eq!(sent(&busy), [(key(from), Message::Decline { call_id: id(80 + from as u8), why: Why::Busy })],
                "a bystander or a town-wide ring from Ada gets Busy");
            assert_eq!(ana.calling_peers(TOWN), [key(1), key(2)], "glare must preserve the targeted ring");
            assert!(!busy.contains(&Mic(true)) && ana.view().phase == Calling, "mismatched glare leaves the mic closed");
        }
        received(&mut ana, 1, &answer_message(session, TOWN));
        let join = join_message(session, TOWN);
        assert!(ana.view().phase == Live && received(&mut ana, 3, &join).is_empty(), "a bystander cannot join the live caller");
        assert_eq!(ana.ignored().stale, 1, "the bystander's join is counted as stale");
        assert_eq!(ana.voice_peers().collect::<Vec<_>>(), [key(1)], "only the answering device has voice");
        let allow = received(&mut ana, 2, &join);
        assert!(allow.contains(&voice(2, true)) && sent(&allow) == [(key(2), join_ack_message(session, false, false,
            CallScope::Seats { caller: [4; 32], callee: ada }))], "Ada's other device joins with a scoped acknowledgement");

        let mut late = seat_flow(2, &[4]);
        let joining = pressed(&mut late, Press::JoinCall(session, TOWN, CallScope::Town));
        assert_eq!(sent(&joining), [(key(4), join)], "late join requests the existing session");
        assert!(!joining.iter().any(|e| matches!(e, Effect::Record(_))), "late join never opens another session");
        assert!(received(&mut late, 4, &join_ack_message(session, false, false, CallScope::Town))
            .contains(&Effect::Record(Fact::Join { session, from: key(4), town: TOWN })), "ack records the join");
    }

    #[test]
    fn callee_card_call_refuses_bystander_in_ringing_and_live_while_admitting_own_and_caller_devices() {
        let scope = CallScope::Seats { caller: [4; 32], callee: [1; 32] };
        let other = CallScope::Seats { caller: [3; 32], callee: [1; 32] };
        let mut ana = seat_flow(4, &[1]);
        let ring = sent(&pressed(&mut ana, Press::CallSeat([1; 32]))).pop().unwrap().1;
        let session = ana.session().unwrap();
        let join = join_message(session, TOWN);
        let ack = |scope| join_ack_message(session, false, false, scope);
        for (to, from) in [(3, 4), (5, 4), (1, 2)] {
            let mut recipient = seat_flow(to, &[from]);
            assert!(received(&mut recipient, from, &ring).is_empty() && recipient.view().phase == Idle
                && recipient.ignored().refused == 1, "misrouted and same-seat rings must be refused before ringing");
        }
        let ringing = || {
            let mut ada = seat_flow(1, &[2, 3, 4, 5]);
            received(&mut ada, 4, &ring);
            assert!(ada.view().ringing() && received(&mut ada, 3, &join).is_empty(), "a bystander cannot join a ringing card call");
            assert!(matches!(&ada.state, State::Ringing { joined_peers, .. } if joined_peers.is_empty())
                && ada.ignored().stale == 1, "the rejected join cannot supply a successor");
            ada
        };
        let mut cancelled = ringing();
        received(&mut cancelled, 4, &Message::Cancel { call_id: session });
        assert_eq!(cancelled.view().phase, Idle, "a bystander cannot keep the ring alive after cancellation");
        let mut ada = ringing();
        let answer = pressed(&mut ada, Press::Answer);
        assert_eq!(sent(&answer), [(key(4), answer_message(session, TOWN))], "answer reaches only the caller");
        received(&mut ana, 1, &answer_message(session, TOWN));
        for msg in [&join, &ack(CallScope::Town)] {
            assert!(received(&mut ada, 3, msg).is_empty(), "a bystander gets no effects from a live callee");
        }
        assert_eq!(ada.ignored().stale, 3, "both live messages and the ringing join were refused");
        assert_eq!(ada.voice_peers().collect::<Vec<_>>(), [key(4)], "only Ana and Ada are talking");

        for (device, attempted, linked) in [
            (3, scope, vec![1, 4]), (2, CallScope::Seats { caller: [1; 32], callee: [1; 32] }, vec![1, 4]),
            (2, scope, vec![3]),
        ] {
            let mut late = seat_flow(device, &linked);
            assert!(pressed(&mut late, Press::JoinCall(session, TOWN, attempted)).is_empty() && late.view().phase == Idle,
                "out-of-scope devices, identical seats and zero eligible peers cannot start joining");
        }
        let mut wrong = seat_flow(2, &[4]);
        pressed(&mut wrong, Press::JoinCall(session, TOWN, CallScope::Town));
        assert!(received(&mut wrong, 4, &ack(scope)).is_empty() && wrong.view().phase == Calling && wrong.ignored().stale == 1,
            "a scoped ack cannot open audio on an unscoped join");
        ana.set_open_town(Some((TOWN, [1, 2, 3, 5].map(|i| (key(i), format!("Peer {i}"))).to_vec())), &mut Vec::new());
        for device in [2, 5] {
            let mut late = seat_flow(device, &[1, 3, 4]);
            let dial = pressed(&mut late, Press::JoinCall(session, TOWN, scope));
            assert_eq!(sent(&dial), [(key(1), join_message(session, TOWN)), (key(4), join_message(session, TOWN))], "late join contacts only the two signed seats");
            assert!(!dial.contains(&Mic(true)), "the microphone waits for an eligible acknowledgement");
            for (from, message) in [(3, ack(other)), (3, ack(scope)), (3, ack(CallScope::Town)), (1, ack(other))] {
                assert!(received(&mut late, from, &message).is_empty() && late.voice_peers().next().is_none()
                    && late.view().phase == Calling, "an ineligible sender or mismatched pair cannot open first audio");
            }
            assert_eq!(late.ignored().stale, 4, "every invalid first acknowledgement is counted");
            for (from, peer) in [(4, &mut ana), (1, &mut ada)] {
                let admitted = received(peer, device, &join);
                assert!(admitted.contains(&voice(device, true)) && sent(&admitted) == [(key(device), ack(scope))],
                    "each live participant admits siblings on either seat with a scoped ack");
                let admitted = received(&mut late, from, &ack(scope));
                assert!(admitted.contains(&voice(from, true)), "each acknowledgement from an eligible device opens its voice link");
                if from == 4 {
                    assert!(admitted.contains(&Mic(true)) && late.voice_peers().collect::<Vec<_>>() == [key(4)],
                        "the first authorized ack starts audio with its sender alone");
                }
            }
            assert!(received(&mut late, 3, &join).is_empty(), "late devices continue to enforce the signed pair");
        }
        let mut older_first = seat_flow(2, &[1, 3, 4]);
        pressed(&mut older_first, Press::JoinCall(session, TOWN, scope));
        assert!(received(&mut older_first, 1, &ack(CallScope::Town)).contains(&Mic(true))
            && older_first.voice_peers().collect::<Vec<_>>() == [key(1)], "an older device on the authorized seat can acknowledge first");
        for msg in [&join, &ack(CallScope::Town)] {
            assert!(received(&mut older_first, 3, msg).is_empty(), "the older ack cannot broaden admission");
        }
        assert_eq!(older_first.ignored().stale, 2, "bystander messages are stale before the caller acknowledges");
        received(&mut older_first, 4, &ack(scope));
        assert_eq!(older_first.voice_peers().collect::<Vec<_>>(), [key(1), key(4)], "both authorized devices join across versions");

        let mut caller = seat_flow(4, &[1]);
        let ring = sent(&pressed(&mut caller, Press::Call)).pop().unwrap().1;
        let mut town = seat_flow(1, &[2, 3, 4]);
        received(&mut town, 4, &ring);
        received(&mut town, 3, &join);
        let answered = pressed(&mut town, Press::Answer);
        assert!(answered.contains(&voice(3, true)) && sent(&answered).contains(&(key(3), ack(CallScope::Town))),
            "a town-wide ring admits an unrung member after local Answer");
        assert!(received(&mut town, 2, &join).contains(&voice(2, true)), "a town call continues admitting late town members");
    }

    struct Mesh {
        flows: [CallFlow; 3],
        now: Now,
        mic: [bool; 3],
        wire: VecDeque<(usize, PublicKey, Vec<u8>)>,
        facts: [Vec<Fact>; 3],
    }

    impl Mesh {
        fn new() -> Self {
            let mut flows = std::array::from_fn(|i| CallFlow::new(Box::new(move |bytes| bytes.fill(10 + i as u8))));
            for (i, flow) in flows.iter_mut().enumerate() {
                flow.set_local_key(key(i));
                flow.set_open_town(Some((TOWN, (0..3).filter(|j| *j != i)
                    .map(|j| (key(j), format!("Peer {j}"))).collect())), &mut Vec::new());
            }
            Self { flows, now: Now { mono_us: 37 }, mic: [false; 3], wire: VecDeque::new(), facts: Default::default() }
        }

        fn apply(&mut self, i: usize, input: impl FnOnce(&mut CallFlow, Now, &mut Vec<Effect>)) -> Vec<Effect> {
            let mut out = Vec::new();
            input(&mut self.flows[i], self.now, &mut out);
            for effect in &out {
                match effect {
                    Effect::SendTo { peer, body } => self.wire.push_back((i, *peer, body.clone())),
                    Effect::Record(fact) => self.facts[i].push(fact.clone()),
                    Effect::Mic(on) => self.mic[i] = *on,
                    _ => {}
                }
            }
            out
        }

        fn press(&mut self, i: usize, press: Press) -> Vec<Effect> {
            self.apply(i, |f, now, out| f.press(press, now, out))
        }

        fn deliver(&mut self, from: usize, to: usize, expected: &Message) -> Vec<Effect> {
            let at = self.wire.iter().position(|(i, peer, body)| *i == from && *peer == key(to) &&
                decode(body).unwrap().as_ref() == Some(expected)).expect("the named message must reach the intended peer");
            let (_, _, body) = self.wire.remove(at).unwrap();
            self.apply(to, |f, now, out| f.receive(key(from), &body, now, out))
        }

        fn ring_both(&mut self) {
            self.press(0, Press::Call);
            let ring = Message::Ring { call_id: id(10), secret: [0; 32], video: false,
                ext: RingExt { town: Some(TOWN), roster: vec![key(1), key(2)], media_caps: None, target_seat: None } };
            for i in [1, 2] { self.deliver(0, i, &ring); }
            assert_eq!(self.mic, [false; 3], "dialling and ringing leave every mic closed");
            self.press(1, Press::Answer);
            self.deliver(1, 0, &answer_message(id(10), TOWN));
            assert_eq!(self.deliver(1, 2, &join_message(id(10), TOWN)), [Changed], "an unanswered join only updates the roster");
            assert!(!self.mic[2] && self.flows[2].view().ringing(), "the last recipient still has to answer");
        }

        fn peers(&self, i: usize) -> Vec<PublicKey> { self.flows[i].voice_peers().collect() }
    }

    fn ack(call_id: CallId) -> Message { Message::CallJoinAck { call_id, muted: false, transcribing: false, seats: None } }

    #[test]
    fn three_party_call_full_mesh_answer_and_partial_hangup() {
        let mut m = Mesh::new();
        m.ring_both();
        m.press(2, Press::Answer);
        m.deliver(2, 0, &answer_message(id(10), TOWN));
        m.deliver(2, 1, &ack(id(10)));
        assert_eq!(m.mic, [true; 3], "all participants have answered");
        for i in 0..3 {
            assert_eq!(m.peers(i), (0..3).filter(|j| *j != i).map(key).collect::<Vec<_>>(), "seat {i} hears both partners");
        }
        let end = Message::End { call_id: id(10) };
        m.press(0, Press::HangUp);
        for i in [1, 2] { m.deliver(0, i, &end); }
        assert_eq!((m.peers(1), m.peers(2)), (vec![key(2)], vec![key(1)]), "survivors keep each other");
        assert_eq!(m.mic, [false, true, true], "one hangup closes only one mic");
        m.press(1, Press::HangUp);
        m.deliver(1, 2, &end);
        assert_eq!(m.flows[2].view().phase, Idle, "the last peer's departure closes the call");
    }

    #[test]
    fn ringing_survives_caller_hangup_when_another_peer_joined() {
        let end = Message::End { call_id: id(10) };
        for caller_reaches_b_first in [false, true] {
            let mut m = Mesh::new();
            m.ring_both();
            m.press(0, Press::HangUp);
            if caller_reaches_b_first {
                m.deliver(0, 1, &end);
                assert!(m.peers(1).is_empty() && m.mic[1] && m.flows[1].view().phase == Live, "B waits alone for C");
                assert!(m.wire.iter().all(|(from, _, _)| *from != 1), "B sends no End while C can answer");
            }
            m.deliver(0, 2, &end);
            assert_eq!(m.flows[2].ringing_from(), Some(key(1)), "B takes over the ring");
            assert!(!m.mic[2], "promoting a caller never answers");
            m.press(2, Press::Answer);
            assert_eq!(m.facts[2], [Fact::Join { session: id(10), town: TOWN, from: key(1) }], "C answers the surviving peer");
            m.deliver(2, 1, &ack(id(10)));
            if !caller_reaches_b_first { m.deliver(0, 1, &end); }
            assert_eq!((m.flows[0].view().phase, m.peers(1), m.peers(2)), (Idle, vec![key(2)], vec![key(1)]), "both orders leave the same call");
            assert!(m.wire.is_empty(), "C only acknowledges B");
            m.press(1, Press::HangUp);
            m.deliver(1, 2, &end);
            assert_eq!(m.flows[2].view().phase, Idle, "B's hangup ends the surviving call");
        }
        for ending in [Some(Press::Decline), Some(Press::HangUp), None] {
            let mut m = Mesh::new();
            m.ring_both();
            m.press(0, Press::HangUp);
            for i in [1, 2] { m.deliver(0, i, &end); }
            match ending {
                Some(Press::Decline) => {
                    m.press(2, Press::Decline);
                    m.deliver(2, 1, &Message::Decline { call_id: id(10), why: Why::Declined });
                }
                _ => {
                    if ending.is_some() { m.press(1, Press::HangUp); }
                    else {
                        m.now.mono_us += RING_WAIT;
                        m.apply(1, CallFlow::tick);
                    }
                    m.deliver(1, 2, &end);
                }
            }
            assert!(m.flows.iter().all(|f| f.view().phase == Idle) && m.wire.is_empty(), "no ringing peer is stranded at {ending:?}");
            assert!(matches!(m.facts[1].last(), Some(Fact::Leave { why: LeaveWhy::Ended, .. })), "B records a normal departure");
            assert!(!m.mic[2] && !m.facts[2].iter().any(|f| matches!(f, Fact::Join { .. })), "C never answered");
        }
    }

    #[test]
    fn late_join_waits_for_ack_before_opening_mic() {
        let mut m = Mesh::new();
        m.flows[0].set_open_town(Some((TOWN, vec![(key(1), "B".into())])), &mut Vec::new());
        m.press(0, Press::Call);
        m.deliver(0, 1, &Message::Ring { call_id: id(10), secret: [0; 32], video: false,
            ext: RingExt { town: Some(TOWN), roster: vec![key(1)], media_caps: None, target_seat: None } });
        m.press(1, Press::Answer);
        m.deliver(1, 0, &answer_message(id(10), TOWN));
        m.flows[0].set_open_town(Some((TOWN, vec![(key(1), "B".into()), (key(2), "C".into())])), &mut Vec::new());
        m.press(2, Press::JoinCall(id(10), TOWN, CallScope::Town));
        for i in [0, 1] { m.deliver(2, i, &join_message(id(10), TOWN)); }
        assert!(m.facts[2].is_empty() && !m.mic[2], "requesting a late join neither records an open nor starts capture");
        m.deliver(0, 2, &ack(id(10)));
        assert!(m.mic[2] && m.peers(2) == [key(0)], "first ack admits the late joiner");
        assert_eq!(m.facts[2], [Fact::Join { session: id(10), town: TOWN, from: key(0) }], "only the first ack records a join");
        assert!(matches!(&m.flows[2].state, State::Live { ringing, .. } if ringing.is_empty()), "a late joiner has rung nobody");
        m.deliver(1, 2, &ack(id(10)));
        assert_eq!(m.peers(2), [key(0), key(1)], "the second ack adds its peer");
    }

    #[test]
    fn crossed_late_joins_wait_for_ack_before_opening_mic() {
        let session = id(21);
        for order in 0..3 {
            let mut m = Mesh::new();
            for (i, other) in [(1, 2), (2, 1)] {
                m.flows[i].set_open_town(Some((TOWN, vec![(key(other), "Peer".into())])), &mut Vec::new());
                m.press(i, Press::JoinCall(session, TOWN, CallScope::Town));
            }
            for (peer, msg) in [(0, join_message(session, TOWN)), (0, ack(session)), (2, join_message(session, [0; 32]))] {
                assert!(m.apply(1, |f,n,out| f.receive(key(peer), &msg.encode(), n, out)).is_empty(), "unattributed or wrong-town messages have no effects");
            }
            assert_eq!(m.flows[1].ignored().stale, 3, "each invalid late-join message was rejected");
            if order == 2 {
                assert_eq!(m.press(1, Press::Cancel), [send(2, Message::End { call_id: session }), Changed], "cancel withdraws the request");
            }
            assert_eq!(m.deliver(1, 2, &join_message(session, TOWN)), [send(1, ack(session))], "a crossed join is only acknowledged");
            assert_eq!(m.mic, [false; 3], "no crossed join opens a mic");
            if order == 2 {
                assert_eq!(m.deliver(1, 2, &Message::End { call_id: session }), [Changed], "withdrawal clears the waiting join");
                m.deliver(2, 1, &join_message(session, TOWN));
                m.deliver(2, 1, &ack(session));
                assert!(m.facts.iter().all(Vec::is_empty) && m.mic == [false; 3], "a cancelled late join records no participation");
                assert_eq!((m.flows[1].view().phase, m.flows[2].view().phase), (Idle, Idle), "neither late message reopens the call");
                continue;
            }
            if order == 1 { m.deliver(2, 1, &ack(session)); }
            let effects = m.deliver(2, 1, &join_message(session, TOWN));
            if order == 1 {
                assert_eq!(effects, [send(2, ack(session)), Changed], "an already live peer acknowledges without adding voice again");
            } else {
                assert_eq!(effects, [send(2, ack(session))], "the second crossed request also waits");
                assert!(!m.mic[1] && m.facts[1].is_empty(), "B has not received consent to connect");
                m.deliver(2, 1, &ack(session));
            }
            m.deliver(1, 2, &ack(session));
            for (i, other) in [(1, 2), (2, 1)] {
                assert!(m.mic[i] && m.peers(i) == [key(other)], "seat {i} joined after an ack");
                assert_eq!(m.facts[i], [Fact::Join { session, town: TOWN, from: key(other) }], "seat {i} records one attributed join");
            }
        }
    }

    #[test]
    fn same_town_glare_lower_key_wins_and_cancels_losing_ring() {
        let mut m = Mesh::new();
        m.press(0, Press::Call);
        m.press(1, Press::Call);
        m.deliver(0, 1, &Message::Ring { call_id: id(10), secret: [0; 32], video: false,
            ext: RingExt { town: Some(TOWN), roster: vec![key(1), key(2)], media_caps: None, target_seat: None } });
        assert_eq!(m.facts[1][1..], [leave(11, None, LeaveWhy::Ended), Fact::Join { session: id(10), from: key(0), town: TOWN }], "the higher key leaves its outgoing call");
        m.deliver(1, 2, &Message::End { call_id: id(11) });
        m.deliver(1, 0, &answer_message(id(10), TOWN));
        m.deliver(1, 2, &join_message(id(10), TOWN));
        assert_eq!(m.flows[1].session(), Some(id(10)), "the winner's ID owns the live call");
    }

    #[test]
    fn foreign_town_carrier_is_refused_across_ringing_glare_and_live() {
        let mut m = Mesh::new();
        let other = [0xbb; 32];
        let session = id(42);
        let ring = Message::Ring { call_id: session, secret: [0; 32], video: false,
            ext: RingExt { town: Some(TOWN), roster: vec![key(2), key(3)], media_caps: None, target_seat: None } };
        let receive = |m: &mut Mesh, sender, msg: &Message| {
            m.apply(0, |flow, now, out| flow.receive(key(sender), &msg.encode(), now, out))
        };
        m.flows[0].set_town_peers(BTreeMap::from([
            (TOWN, BTreeSet::from([key(1), key(2)])), (other, BTreeSet::from([key(3)])),
        ]), m.now, &mut Vec::new());
        assert!(receive(&mut m, 3, &ring).is_empty(), "a foreign ring neither replies nor starts ringing");
        assert_eq!((m.flows[0].view().phase, m.flows[0].ignored().refused), (Idle, 1), "a foreign ring is refused at idle");
        receive(&mut m, 1, &ring);
        assert!(matches!(&m.flows[0].state, State::Ringing { roster, .. } if *roster == BTreeSet::from([key(2)])),
            "only Kit is eligible in Ben's roster");
        assert!(receive(&mut m, 3, &join_message(session, TOWN)).is_empty(), "a foreign join has no ringing effects");
        assert_eq!(m.flows[0].ignored().stale, 1, "ringing refused the foreign join");
        let effects = m.press(0, Press::Answer);
        let sent: Vec<_> = effects.into_iter().filter(|e| matches!(e, Effect::SendTo { .. })).collect();
        assert_eq!(sent, [send(1, answer_message(session, TOWN)), send(2, join_message(session, TOWN))],
            "answer signals only the caller and the eligible roster");
        assert_eq!(m.peers(0), [key(1)], "only the caller has voice at answer");
        m.flows[0].set_open_town(Some((other, vec![(key(3), "Eve".into())])), &mut Vec::new());
        for msg in [ack(session), join_message(session, TOWN), join_message(session, other), answer_message(session, TOWN)] {
            assert!(receive(&mut m, 3, &msg).is_empty(), "opening another town cannot admit its peer via {msg:?}");
        }
        assert_eq!(m.flows[0].ignored().stale, 5, "all four foreign live messages were refused");
        assert_eq!(m.peers(0), [key(1)], "the call still contains only Ben");
        assert_eq!(receive(&mut m, 2, &ack(session)), [voice(2, true), Changed], "the background call still admits Kit");
        assert_eq!(m.peers(0), [key(1), key(2)], "both authorized peers have voice");
        let out = m.apply(0, |flow, now, out| flow.set_town_peers(BTreeMap::from([
            (TOWN, BTreeSet::from([key(1)])), (other, BTreeSet::from([key(3)])),
        ]), now, out));
        assert_eq!(out, [voice(2, false), send(2, Message::End { call_id: session }), Changed], "revocation closes only Kit's voice");
        for msg in [join_message(session, TOWN), ack(session)] {
            assert!(receive(&mut m, 2, &msg).is_empty(), "a revoked peer cannot rejoin");
        }
        assert_eq!(m.peers(0), [key(1)], "Ben remains live after the revocation");

        // Give the foreign caller the lower key, so admission alone prevents glare folding.
        m.press(0, Press::HangUp);
        m.flows[0].set_local_key(key(4));
        m.flows[0].set_open_town(Some((TOWN, vec![(key(1), "Ben".into())])), &mut Vec::new());
        m.press(0, Press::Call);
        m.flows[0].set_open_town(Some((other, vec![(key(3), "Eve".into())])), &mut Vec::new());
        let glare = Message::Ring { call_id: id(43), secret: [0; 32], video: false,
            ext: RingExt { town: Some(TOWN), ..Default::default() } };
        assert!(receive(&mut m, 3, &glare).is_empty(), "a lower-key foreign caller cannot win glare");
        assert_eq!(m.flows[0].ignored().refused, 2, "foreign glare counts as a refused ring");
        assert_eq!((m.flows[0].view().phase, m.flows[0].active_town(), m.mic[0]), (Calling, Some(TOWN), false),
            "the original outgoing call keeps waiting with its mic closed");
    }

    #[test]
    fn different_town_ring_while_calling_replies_busy() {
        let mut m = Mesh::new();
        m.press(0, Press::Call);
        let town = [7; 32];
        m.flows[0].set_town_peers(BTreeMap::from([(TOWN, BTreeSet::from([key(1), key(2)])), (town, BTreeSet::from([key(1)]))]), m.now, &mut Vec::new());
        let ring = Message::Ring { call_id: id(11), secret: [0; 32], video: false, ext: RingExt { town: Some(town), ..Default::default() } };
        let out = m.apply(0, |f, now, out| f.receive(key(1), &ring.encode(), now, out));
        assert!(out.contains(&send(1, Message::Decline { call_id: id(11), why: Why::Busy })), "a different town gets a busy reply");
        assert_eq!(m.facts[0].last(), Some(&Fact::Decline { session: id(11), why: DeclineWhy::Busy, from: key(1), town }), "busy belongs to the incoming town and peer");
        assert_eq!((m.flows[0].view().phase, m.flows[0].active_town(), m.mic[0]), (Calling, Some(TOWN), false), "the original town is still ringing");
    }
}
