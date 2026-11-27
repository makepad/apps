//! One linked peer and one call at a time. Each input appends its effects in execution order.
use super::signal::{decode, CallId, Message, Secret, Why};
use makepad_town_core::{event::{LeaveWhy, DeclineWhy}, key::PublicKey};
use std::collections::HashSet;

/// Microseconds to give a person time to answer a ring.
pub const RING_WAIT: u64 = 30_000_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Now {
    pub mono_us: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Press {
    Call,
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
    LetControl,
    AllowAll(bool),
    RevokeControl(PublicKey),
    TakeBack,
    ReleaseControl,
}

#[derive(Eq, PartialEq)]
#[cfg_attr(test, derive(Debug))]
pub enum Effect {
    Record(Fact),
    Send(Vec<u8>),
    Start { call_id: CallId, send_secret: Secret, recv_secret: Secret, caller: bool },
    Stop,
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
    Open { session: CallId, video: bool, to: PublicKey },
    Join { session: CallId },
    Leave { session: CallId, secs: Option<u32>, why: LeaveWhy },
    Decline { session: CallId, why: DeclineWhy },
    ShareOpen { session: CallId, title: String, to: PublicKey },
    ShareJoin { session: CallId },
}

#[derive(Clone, Debug, Default)]
pub struct CallView {
    pub phase: Phase,
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

// Secrets exist in the pending handshake only. The view can be cloned for the widgets.
enum State {
    Idle,
    Calling { call_id: CallId, send_secret: Secret },
    Ringing { call_id: CallId, their_secret: Secret },
    Live { call_id: CallId },
}

impl State {
    fn id(&self) -> Option<CallId> {
        match self {
            Self::Idle => None,
            Self::Calling { call_id, .. } | Self::Ringing { call_id, .. } | Self::Live { call_id, .. } => {
                Some(*call_id)
            }
        }
    }
}

pub struct CallFlow {
    random: Box<dyn FnMut(&mut [u8]) + Send>,
    peer: Option<PublicKey>,
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
            peer: None,
            state: State::Idle,
            view: CallView::default(),
            ended: HashSet::new(),
            ignored: Ignored::default(),
            answered_transcribe: false, origin_us: 0,
        }
    }

    pub fn view(&self) -> &CallView {
        &self.view
    }
    pub fn peer(&self) -> Option<&PublicKey> {
        self.peer.as_ref()
    }
    pub fn ignored(&self) -> Ignored {
        self.ignored
    }

    pub fn linked(&mut self, peer: PublicKey, name: &str, now: Now, out: &mut Vec<Effect>) {
        if self.peer == Some(peer) && self.view.peer_name == name {
            return;
        }
        if self.peer != Some(peer) {
            self.link_lost(now, out);
        }
        self.peer = Some(peer);
        self.view.peer_name = name.to_owned();
        out.push(Effect::Changed);
    }

    pub fn unlinked(&mut self, now: Now, out: &mut Vec<Effect>) {
        if self.peer.take().is_none() {
            return;
        }
        self.link_lost(now, out);
        self.view.peer_name.clear();
        out.push(Effect::Changed);
    }

    fn link_lost(&mut self, now: Now, out: &mut Vec<Effect>) {
        let why = if self.view.phase == Phase::Live { LeaveWhy::Dropped } else { LeaveWhy::Failed };
        self.finish(now, why, out);
    }

    pub fn session(&self) -> Option<CallId> { self.state.id() }

    fn secs(&self, now: Now) -> u64 {
        now.mono_us.saturating_sub(self.view.since.expect("active call has a start time")) / 1_000_000
    }

    fn finish(&mut self, now: Now, why: LeaveWhy, out: &mut Vec<Effect>) {
        let Some(session) = self.state.id() else { return };
        if self.view.transcribing_me { out.push(Effect::Transcribe(false)); }
        self.view.transcribing_me = false;
        self.view.peer_transcribing = false;
        self.view.transcribe_prompt = false;
        self.answered_transcribe = false;
        match self.state {
            State::Idle => return,
            State::Calling { .. } => out.push(Effect::Record(Fact::Leave { session, secs: None, why })),
            State::Ringing { .. } => out.extend([Effect::Ring(false), Effect::Speaker(false)]),
            State::Live { .. } => {
                let secs = (why != LeaveWhy::Failed).then(|| u32::try_from(self.secs(now)).unwrap_or(u32::MAX));
                out.extend([Effect::Stop, Effect::Mic(false), Effect::Speaker(false),
                    Effect::Record(Fact::Leave { session, secs, why })]);
            }
        }
        self.ended.insert(session);
        self.state = State::Idle;
        self.view.phase = Phase::Idle;
        self.view.since = None;
        self.view.duration_secs = 0;
        self.view.muted = false;
        self.view.unmuting = false;
        self.view.peer_muted = false;
    }

    fn join(
        &mut self,
        call_id: CallId,
        send_secret: Secret,
        recv_secret: Secret,
        caller: bool,
        now: Now,
        out: &mut Vec<Effect>,
    ) {
        if self.view.ringing() {
            out.push(Effect::Ring(false));
        }
        out.push(Effect::Start { call_id, send_secret, recv_secret, caller });
        // An incoming ring already opened the speaker. A crossed outgoing ring was silent.
        if self.view.phase == Phase::Calling {
            out.push(Effect::Speaker(true));
        }
        out.push(Effect::Mic(true));
        if !caller {
            out.push(Effect::Send(Message::Answer { call_id, secret: send_secret, ext: Default::default() }.encode()));
        }
        self.state = State::Live { call_id };
        self.view.phase = Phase::Live;
        self.view.since = Some(now.mono_us);
        if !caller { out.push(Effect::Record(Fact::Join { session: call_id })); }
    }

    fn answer(&mut self, call_id: CallId, their_secret: Secret, now: Now, out: &mut Vec<Effect>) {
        let mut ours = [0; 32];
        (self.random)(&mut ours);
        self.join(call_id, ours, their_secret, false, now, out);
    }

    fn cancel(&mut self, call_id: CallId, now: Now, out: &mut Vec<Effect>) {
        out.push(Effect::Send(Message::Cancel { call_id }.encode()));
        self.finish(now, LeaveWhy::Ended, out);
    }

    pub fn press(&mut self, press: Press, now: Now, out: &mut Vec<Effect>) {
        match (&self.state, press) {
            (State::Idle, Press::Call) if self.peer.is_some() => {
                let mut call_id = [0; 16];
                (self.random)(&mut call_id);
                while self.ended.contains(&call_id) {
                    (self.random)(&mut call_id);
                }
                let mut secret = [0; 32];
                (self.random)(&mut secret);
                out.push(Effect::Record(Fact::Open { session: call_id, video: false, to: self.peer.unwrap() }));
                self.view.phase = Phase::Calling;
                self.view.since = Some(now.mono_us);
                self.origin_us = now.mono_us;
                self.state = State::Calling { call_id, send_secret: secret };
                out.push(Effect::Send(Message::Ring { call_id, secret, video: false, ext: Default::default() }.encode()));
            }
            (State::Ringing { call_id, their_secret }, Press::Answer) => self.answer(*call_id, *their_secret, now, out),
            (State::Ringing { call_id, .. }, Press::Decline | Press::HangUp) => {
                out.push(Effect::Send(Message::Decline { call_id: *call_id, why: Why::Declined }.encode()));
                out.push(Effect::Record(Fact::Decline { session: *call_id, why: DeclineWhy::Declined }));
                self.finish(now, LeaveWhy::Ended, out);
            }
            (State::Calling { call_id, .. }, Press::Cancel | Press::HangUp) => self.cancel(*call_id, now, out),
            (State::Live { call_id, .. }, Press::HangUp) => {
                let end = Message::End { call_id: *call_id };
                self.finish(now, LeaveWhy::Ended, out);
                out.push(Effect::Send(end.encode()));
            }
            (State::Live { call_id }, Press::Transcribe | Press::StopTranscribing) => {
                let on = press == Press::Transcribe;
                if on == self.view.transcribing_me { return }
                self.view.transcribing_me = on;
                self.answered_transcribe = true;
                self.view.transcribe_prompt = false;
                out.push(Effect::Transcribe(on));
                out.push(Effect::Send(Message::Transcribe { call_id: *call_id, on }.encode()));
            }
            (State::Live { .. }, Press::NotMe) if self.view.transcribe_prompt => {
                self.answered_transcribe = true;
                self.view.transcribe_prompt = false;
            }
            (State::Live { call_id, .. }, Press::Mute | Press::Unmute) => {
                let on = press == Press::Mute;
                if self.view.muted == on {
                    return;
                }
                self.view.muted = on;
                self.view.unmuting = !on;
                out.push(Effect::Mic(!on));
                out.push(Effect::Send(Message::Mute { call_id: *call_id, on }.encode()));
            }
            _ => return,
        }
        out.push(Effect::Changed);
    }

    pub fn message(&mut self, body: &[u8], now: Now, out: &mut Vec<Effect>) {
        let message = match decode(body) {
            Ok(Some(Message::Share { .. } | Message::ShareOff { .. } | Message::View { .. }
                | Message::CallJoin { .. } | Message::CallJoinAck { .. } | Message::MediaCapsNotice { .. }
                | Message::RecordingState { .. } | Message::RecordingConsent { .. })) | Ok(None) => return,
            Ok(Some(message)) => message,
            Err(_) => {
                self.ignored.refused += 1;
                return;
            }
        };
        let Some(id) = message.call_id() else { return };
        if self.peer.is_none() || self.ended.contains(&id) {
            self.ignored.stale += 1;
            return;
        }
        if let Message::Ring { secret, .. } = message {
            self.ring(id, secret, now, out);
            return;
        }
        if self.state.id() != Some(id) {
            self.ignored.stale += 1;
            return;
        }
        match message {
            Message::Answer { secret, .. } => {
                let State::Calling { send_secret, .. } = self.state else {
                    self.ignored.stale += 1;
                    return;
                };
                self.join(id, send_secret, secret, true, now, out);
            }
            Message::Mute { on, .. } => {
                if self.view.phase != Phase::Live {
                    self.ignored.stale += 1;
                    return;
                }
                if self.view.peer_muted == on {
                    return;
                }
                self.view.peer_muted = on;
            }
            Message::Transcribe { on, .. } => {
                if self.view.phase != Phase::Live { self.ignored.stale += 1; return }
                if self.view.peer_transcribing == on { return }
                self.view.peer_transcribing = on;
                self.view.transcribe_prompt = on && !self.view.transcribing_me && !self.answered_transcribe;
            }
            Message::End { .. } => {
                let why = if self.view.phase == Phase::Live { LeaveWhy::Ended } else { LeaveWhy::Failed };
                self.finish(now, why, out);
            }
            Message::Cancel { .. } | Message::Decline { .. } => {
                let why = if self.view.phase == Phase::Live { LeaveWhy::Failed } else { LeaveWhy::Ended };
                self.finish(now, why, out);
            }
            Message::Ring { .. } => unreachable!(),
            _ => return,
        }
        out.push(Effect::Changed);
    }

    fn ring(&mut self, id: CallId, secret: Secret, now: Now, out: &mut Vec<Effect>) {
        match self.state {
            State::Idle => {
                self.origin_us = now.mono_us;
                self.state = State::Ringing { call_id: id, their_secret: secret };
                self.view.phase = Phase::Ringing;
                self.view.since = Some(now.mono_us);
                out.extend([Effect::Speaker(true), Effect::Ring(true)]);
            }
            State::Calling { call_id, send_secret } => {
                // The secret breaks an ID tie. Identical bodies cannot distinguish a caller:
                // neither microphone opens, and the ordinary ring timeout still applies.
                if (call_id, send_secret) <= (id, secret) {
                    if call_id != id {
                        self.ended.insert(id);
                    } else {
                        self.ignored.stale += 1;
                    }
                    return;
                }
                if call_id != id {
                    self.ended.insert(call_id);
                    out.push(Effect::Record(Fact::Leave { session: call_id, secs: None, why: LeaveWhy::Ended }));
                }
                if call_id != id { self.origin_us = now.mono_us; }
                self.answer(id, secret, now, out);
            }
            _ if self.state.id() == Some(id) => {
                self.ignored.stale += 1;
                return;
            }
            _ => {
                self.ended.insert(id);
                out.push(Effect::Record(Fact::Decline { session: id, why: DeclineWhy::Busy }));
                out.push(Effect::Send(Message::Decline { call_id: id, why: Why::Busy }.encode()));
            }
        }
        out.push(Effect::Changed);
    }

    pub fn tick(&mut self, now: Now, out: &mut Vec<Effect>) {
        let Some(since) = self.view.since else {
            return;
        };
        match self.state {
            State::Calling { call_id, .. } if now.mono_us.saturating_sub(since) >= RING_WAIT => {
                self.cancel(call_id, now, out)
            }
            State::Ringing { .. } if now.mono_us.saturating_sub(since) >= RING_WAIT => {
                self.finish(now, LeaveWhy::Ended, out)
            }
            State::Live { .. } => {
                let secs = self.secs(now);
                if self.view.duration_secs == secs {
                    return;
                }
                self.view.duration_secs = secs;
            }
            _ => return,
        }
        out.push(Effect::Changed);
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
    use Effect::{Changed, Mic, Ring, Speaker, Stop};
    use Phase::{Calling, Idle, Live, Ringing};

    const NAMES: [&str; 2] = ["East", "West"];
    fn leave(byte: u8, secs: Option<u32>, why: LeaveWhy) -> Fact { Fact::Leave { session: id(byte), secs, why } }
    fn decline(byte: u8, why: DeclineWhy) -> Fact { Fact::Decline { session: id(byte), why } }
    fn id(byte: u8) -> CallId {
        [byte; 16]
    }
    fn send(msg: Message) -> Effect {
        Effect::Send(msg.encode())
    }
    fn start(id_byte: u8, ours: u8, theirs: u8, caller: bool) -> Effect {
        Effect::Start { call_id: id(id_byte), send_secret: [ours; 32], recv_secret: [theirs; 32], caller }
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
                p.link(i);
            }
            p
        }

        fn link(&mut self, i: usize) {
            let peer = 1 - i;
            self.input(i, &[Changed], Idle, |ctl, now, fx| {
                ctl.linked([peer as u8; 65], NAMES[peer], now, fx)
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
                    Effect::Send(bytes) => side.wire.push_back(bytes),
                    Effect::Start { .. } => {
                        assert!(!side.media);
                        side.media = true;
                    }
                    Stop => {
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
            self.input(i, fx, phase, |c, n, out| c.message(body, n, out));
        }
        fn deliver(&mut self, from: usize, fx: &[Effect], phase: Phase) {
            let body = self.sides[from].wire.pop_front().expect("a message is pending");
            self.receive(1 - from, &body, fx, phase);
        }
        fn tick(&mut self, i: usize, fx: &[Effect], phase: Phase) {
            self.input(i, fx, phase, CallFlow::tick);
        }
        fn unlink(&mut self, i: usize, fx: &[Effect]) {
            self.input(i, fx, Idle, CallFlow::unlinked);
            assert_eq!(self.sides[i].ctl.peer(), None);
        }
        fn flowing(&mut self, i: usize, fx: &[Effect]) {
            self.input(i, fx, Live, CallFlow::mic_flowing);
        }
        fn dial(&mut self, i: usize, byte: u8) {
            self.press(
                i,
                Press::Call,
                &[send(Message::Ring { call_id: id(byte), secret: [byte + 1; 32], video: false, ext: Default::default() }), Changed],
                Calling,
            );
            assert_eq!(self.sides[i].facts.last(), Some(&Fact::Open { session: id(byte), video: false, to: [(1-i) as u8; 65] }), "caller records its own ring");
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
                    start(10, 80, 11, false),
                    Mic(true),
                    send(Message::Answer { call_id: id(10), secret: [80; 32], ext: Default::default() }),
                    Changed,
                ],
                Live,
            );
        }
        fn answer(&mut self) {
            self.answer_press();
            self.deliver(1, &[start(10, 11, 80, true), Speaker(true), Mic(true), Changed], Live);
            assert_eq!(self.sides[1].facts, [Fact::Join { session: id(10) }], "answer signs the callee’s join");
        }
        fn hang(&mut self, i: usize, byte: u8) {
            self.press(
                i,
                Press::HangUp,
                &[Stop, Mic(false), Speaker(false), send(Message::End { call_id: id(byte) }), Changed],
                Idle,
            );
        }
        fn hanging_up(&mut self, i: usize, byte: u8) {
            self.hang(i, byte);
            self.deliver(i, &[Stop, Mic(false), Speaker(false), Changed], Idle);
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
                &[send(Message::Decline { call_id: id(10), why: Why::Declined }), Ring(false), Speaker(false), Changed],
                Idle,
            );
            p.deliver(1, &[Changed], Idle);
            assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller closes the ring");
            assert_eq!(p.sides[1].facts, [decline(10, DeclineWhy::Declined)], "callee signs the decline");
            p.closed();
        }
    }

    #[test]
    fn caller_cancels_and_peer_misses_the_call() {
        let mut p = Pair::new();
        p.ringing();
        p.press(0, Press::Cancel, &[send(Message::Cancel { call_id: id(10) }), Changed], Idle);
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
            p.tick(0, &[send(Message::Cancel { call_id: id(10) }), Changed], Idle);
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
        for caller in 0..2 {
            let callee = 1 - caller;
            for first in 0..2 {
                let mut p = if caller == 0 { Pair::new() } else { Pair::with_seeds(80, 10) };
                p.dial(caller, 10);
                p.dial(callee, 80);
                for from in [first, 1 - first] {
                    if from == caller {
                        p.deliver(
                            caller,
                            &[
                                start(10, 82, 11, false),
                                Speaker(true),
                                Mic(true),
                                send(Message::Answer { call_id: id(10), secret: [82; 32], ext: Default::default() }),
                                Changed,
                            ],
                            Live,
                        );
                    } else {
                        p.deliver(callee, &[], Calling);
                    }
                }
                p.deliver(callee, &[start(10, 11, 82, true), Speaker(true), Mic(true), Changed], Live);
                assert_eq!(&p.sides[callee].facts[1..], &[leave(80, None, LeaveWhy::Ended), Fact::Join { session: id(10) }], "crossed caller ends its own ring and joins the winning session");
                p.hanging_up(callee, 10);
                for side in &p.sides { assert_eq!(side.facts.last(), Some(&leave(10, Some(0), LeaveWhy::Ended)), "each participant records its duration"); }
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
        p.unlink(1, &[Stop, Mic(false), Speaker(false), Changed]);
        p.link(1);
        p.advance(2_000_000);
        p.dial(1, 81);
        p.deliver(1, &[send(Message::Decline { call_id: id(81), why: Why::Busy }), Changed], Live);
        p.deliver(0, &[Changed], Idle);
        p.hang(0, 10);
        p.deliver(0, &[], Idle);
        assert_eq!(&p.sides[0].facts[1..], &[decline(81, DeclineWhy::Busy), leave(10, Some(2), LeaveWhy::Ended)], "busy refers to the second ring only");
        assert_eq!(p.sides[1].facts[1], leave(10, Some(0), LeaveWhy::Dropped), "link loss ends the first call");
        assert_eq!(p.sides[1].facts.last(), Some(&leave(81, None, LeaveWhy::Ended)), "the second caller closes its ring");
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
                    p.unlink(i, &[Stop, Mic(false), Speaker(false), Changed]);
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
        let local = |on| [Effect::Transcribe(on), send(msg(on)), Changed];
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
        p.press(0, Press::Transcribe, &local(true), Live);
        p.deliver(0, &[Changed], Live);
        let b = p.sides[1].ctl.view();
        assert!(b.peer_transcribing && b.transcribe_prompt && !b.transcribing_me, "peer announcement only invites");
        p.press(1, Press::NotMe, &[Changed], Live);
        p.press(0, Press::StopTranscribing, &local(false), Live);
        p.deliver(0, &[Changed], Live);
        assert!(!p.sides[1].ctl.view().peer_transcribing);
        p.press(0, Press::Transcribe, &local(true), Live);
        p.deliver(0, &[Changed], Live);
        assert!(!p.sides[1].ctl.view().transcribe_prompt, "Not me lasts through a peer restart");
        p.press(1, Press::Transcribe, &local(true), Live);
        p.deliver(1, &[Changed], Live);
        for s in &p.sides {
            let v = s.ctl.view();
            assert!(v.transcribing_me && v.peer_transcribing && !v.transcribe_prompt);
        }
        for i in 0..2 {
            p.unlink(i, &[Effect::Transcribe(false), Stop, Mic(false), Speaker(false), Changed]);
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
                p.press(i, press, &[Mic(!on), send(Message::Mute { call_id: id(10), on }), Changed], Live);
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
        p.receive(0, &Message::Answer { call_id: id(42), secret: [18; 32], ext: Default::default() }.encode(), &[], Calling);
        p.receive(1, &Message::End { call_id: id(42) }.encode(), &[], Ringing);
        p.answer();
        p.hanging_up(0, 10);
        for i in 0..2 {
            p.receive(i, &Message::Ring { call_id: id(10), secret: [18; 32], video: false, ext: Default::default() }.encode(), &[], Idle);
            p.receive(i, &[1, 99, 0xff], &[], Idle);
            p.receive(i, &Message::CallJoinAck { call_id: id(10), muted: false, transcribing: true }.encode(), &[], Idle);
            assert_eq!(p.sides[i].ctl.ignored(), Ignored { stale: 2, refused: 1 });
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
                        &[Stop, Mic(false), Speaker(false), Changed]
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
        let cancel = [send(Message::Cancel { call_id: id(10) }), Changed];
        if timeout {
            p.tick(0, &cancel, Idle);
        } else {
            p.press(0, Press::Cancel, &cancel, Idle);
        }
        // Both sends are still on the wire. Only the answerer opened devices.
        p.deliver(0, &[Stop, Mic(false), Speaker(false), Changed], Idle);
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
        p.press(0, Press::Cancel, &[send(Message::Cancel { call_id: id(10) }), Changed], Idle);
        p.press(
            1,
            Press::Decline,
            &[send(Message::Decline { call_id: id(10), why: Why::Declined }), Ring(false), Speaker(false), Changed],
            Idle,
        );
        for i in 0..2 {
            p.deliver(i, &[], Idle);
            assert_eq!(p.sides[1 - i].ctl.ignored().stale, 1);
        }
        assert_eq!(p.sides[0].facts.last(), Some(&leave(10, None, LeaveWhy::Ended)), "caller cancels");
        assert_eq!(p.sides[1].facts, [decline(10, DeclineWhy::Declined)], "callee declined independently");
        p.closed();
    }
}
