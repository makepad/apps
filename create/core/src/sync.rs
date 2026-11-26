//! Transport-independent sync for two ancestor-closed copies of one room. Pass one complete message per call.
//! Version 3 starts with the version, kind and 32-byte room id. The rest of a Heads message is consecutive
//! 32-byte ids. The rest of an Events message is consecutive little-endian log records.

use crate::event::{Event, FormatError, Id};
use crate::log::{encode_record, record, Log};
use crate::fold::State;
use crate::key::PublicKey;
use crate::share::{Message, HEADER_BYTES};
use std::collections::{HashSet, VecDeque};
use std::{fmt, io, ops::Range};

pub(crate) const VERSION: u8 = 3;
pub(crate) const HEADS: u8 = 1;
pub(crate) const EVENTS: u8 = 2;
// The previous u32 frame length included the kind byte. The 16 MiB cap comes with L3t.
pub(crate) const MAX_MESSAGE: usize = u32::MAX as usize - 1;
// Split between records. A single larger record gets its own message.
const BATCH_BYTES: usize = 64 * 1024;
const MAX_HEADS: usize = (MAX_MESSAGE - HEADER_BYTES) / 32;

#[derive(Debug)]
pub enum SyncError {
    Version(u8),
    Malformed(&'static str),
    Event(FormatError),
    Append(io::Error),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(v) => write!(f, "unsupported sync version {v}"),
            Self::Malformed(what) => write!(f, "malformed sync message: {what}"),
            Self::Event(e) => write!(f, "invalid sync event: {e}"),
            Self::Append(e) => write!(f, "appending sync event: {e}"),
        }
    }
}

impl std::error::Error for SyncError {}

#[derive(Debug, Default)]
pub struct Received {
    pub appended: Vec<Id>,
    /// Verified membership records delivered by the peer, including already stored records.
    pub delivered_membership: Vec<Id>,
    /// Number of records skipped because at least one parent was absent.
    pub missing_parents: usize,
}

struct Difference {
    known: HashSet<Id>,
    remaining: Range<usize>,
}

#[derive(Default, PartialEq, Eq)]
enum HistoryBase {
    #[default]
    None,
    ResumedGrant,
    PeerHeads,
}

#[derive(Default)]
pub struct SyncEngine {
    heads_pending: bool,
    difference: Option<Difference>,
    live: VecDeque<Id>,
    send_end: Option<usize>,
    shown_final: Option<Id>,
    history_base: HistoryBase,
}

impl SyncEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a fresh connection. Heads are sampled when the message is polled.
    pub fn connect(&mut self, _log: &Log) {
        *self = Self { heads_pending: true, ..Self::default() };
    }

    /// Set the sendable prefix when a link starts or its membership changes. A final
    /// exchange with a removed device includes the removal itself. An uninvited device
    /// has no sendable history, including in its heads.
    pub fn serve(&mut self, log: &Log, state: &State, me: &PublicKey, peer: &PublicKey) -> bool {
        let eligible = |key| state.active_member(key) || state.invitation(key).is_some();
        let end = match state.first_removal(log, me, peer) {
            Some(at) => Some(at + 1),
            None if eligible(me) && eligible(peer) => None,
            None => Some(0),
        };
        // A late membership decision can make previously withheld records eligible.
        // A fresh scan can resend duplicates, which the receiver already verifies and skips.
        let visible = end.unwrap_or(log.events().len());
        if self.history_base != HistoryBase::None && self.send_end.is_some_and(|old| old < visible) {
            self.difference = Some(Difference { known: HashSet::new(), remaining: 0..visible });
        }
        self.send_end = end;
        end != Some(0)
    }

    pub fn shown_final(&self) -> Option<Id> { self.shown_final }

    /// Queue this link's evidence when it first serves a removal-bounded prefix. Its last
    /// event is a head, and the head cap keeps the latest arrivals first.
    pub fn take_final_heads(&mut self, log: &Log) -> Option<Vec<u8>> {
        let event = self.send_end.and_then(|end| end.checked_sub(1))
            .and_then(|at| log.events().get(at)).map(|e| e.id);
        if self.shown_final == event { return None }
        self.shown_final = event;
        event?;
        self.heads_pending = false;
        Some(self.heads_message(log, MAX_HEADS))
    }

    pub fn on_message(&mut self, bytes: &[u8], log: &mut Log) -> Result<Received, SyncError> {
        self.receive(Message::decode(bytes)?, log)
    }

    pub fn receive(&mut self, message: Message<'_>, log: &mut Log) -> Result<Received, SyncError> {
        match message {
            Message::Heads { room, heads } if room == log.room_id() => {
                self.start_difference(heads, log, HistoryBase::PeerHeads);
                Ok(Received::default())
            }
            Message::Events { room, records } if room == log.room_id() => self.receive_events(records, log),
            Message::Heads { .. } | Message::Events { .. } => Err(SyncError::Malformed("wrong sync room")),
            _ => Err(SyncError::Malformed("expected heads or events")),
        }
    }

    /// The founder of a resumed join knows its grant. That starts catch-up until its
    /// own HEADS arrives, which is the authoritative basis for its final exchange.
    pub fn resume_from_grant(&mut self, grant: Id, log: &Log) {
        if self.history_base != HistoryBase::PeerHeads {
            self.start_difference(&[grant], log, HistoryBase::ResumedGrant);
        }
    }

    pub fn peer_heads_answered(&self) -> bool {
        self.history_base == HistoryBase::PeerHeads && self.difference.is_none() && self.live.is_empty()
    }

    fn start_difference(&mut self, heads: &[Id], log: &Log, base: HistoryBase) {
        let mut known = HashSet::new();
        let mut stack = Vec::new();
        for head in heads {
            stack.push(*head);
            while let Some(id) = stack.pop() {
                if self.send_end.is_some_and(|end| log.position(&id).is_none_or(|i| i >= end)) { continue }
                if let Some(event) = log.get(&id) {
                    if known.insert(id) {
                        stack.extend_from_slice(&event.header.parents);
                    }
                }
            }
        }
        self.difference = Some(Difference { known, remaining: 0..log.events().len() });
        // The new scan covers every local notification up to this point.
        self.live.clear();
        self.history_base = base;
    }

    fn receive_events(&mut self, mut payload: &[u8], log: &mut Log) -> Result<Received, SyncError> {
        // Stage accepted records in wire order. Even a duplicate must verify before we commit.
        let mut accepted = Vec::new();
        let mut ids = HashSet::new();
        let mut received = Received::default();
        while !payload.is_empty() {
            let ((header, body, sig), used) = record(payload).ok_or(SyncError::Malformed("incomplete record"))?;
            let event = Event::from_parts(log.room_id(), header.to_vec(), body.to_vec(), sig.to_vec()).map_err(SyncError::Event)?;
            payload = &payload[used..];
            if event.header.kind == crate::event::kind::MEMBER {
                received.delivered_membership.push(event.id);
            }
            if log.get(&event.id).is_some() || ids.contains(&event.id) {
                continue;
            }
            if event.header.parents.iter().any(|p| !ids.contains(p) && log.get(p).is_none()) {
                received.missing_parents += 1;
            } else {
                ids.insert(event.id);
                received.appended.push(event.id);
                accepted.push(event);
            }
        }
        log.check_batch(&accepted).map_err(SyncError::Event)?;
        self.heads_pending |= received.missing_parents != 0;
        log.append_all(accepted).map_err(SyncError::Append)?;
        Ok(received)
    }

    /// Notify in local append order. Appends before the first peer Heads are in its scan.
    pub fn on_local_append(&mut self, event: &Event) {
        if self.history_base != HistoryBase::None {
            self.live.push_back(event.id);
        }
    }

    pub fn poll_send(&mut self, log: &Log) -> Option<Vec<u8>> {
        self.poll_send_with_head_limit(log, MAX_HEADS)
    }

    fn heads_message(&self, log: &Log, max_heads: usize) -> Vec<u8> {
        let mut heads = log.heads_before(self.send_end.unwrap_or(usize::MAX));
        if heads.len() > max_heads {
            // Omitted heads can only make the peer resend records we already have.
            heads.select_nth_unstable_by_key(max_heads, |id| std::cmp::Reverse(log.position(id)));
            heads.truncate(max_heads);
            heads.sort_unstable();
        }
        Message::Heads { room: log.room_id(), heads: &heads }.encode()
    }

    fn poll_send_with_head_limit(&mut self, log: &Log, max_heads: usize) -> Option<Vec<u8>> {
        if std::mem::take(&mut self.heads_pending) {
            return Some(self.heads_message(log, max_heads));
        }

        if let Some(diff) = &mut self.difference {
            let mut batch = Message::Events { room: log.room_id(), records: &[] }.encode();
            if let Some(end) = self.send_end { diff.remaining.end = diff.remaining.end.min(end); }
            while !diff.remaining.is_empty() {
                let event = &log.events()[diff.remaining.start];
                if !diff.known.contains(&event.id) {
                    let len = 10 + event.header_bytes.len() + event.body.len() + event.sig.len();
                    if batch.len() > HEADER_BYTES && len > BATCH_BYTES.saturating_sub(batch.len()) {
                        break;
                    }
                    encode_record(event, &mut batch);
                }
                diff.remaining.start += 1;
            }
            if diff.remaining.is_empty() {
                self.difference = None;
            }
            if batch.len() > HEADER_BYTES {
                return Some(batch);
            }
        }

        while let Some(id) = self.live.pop_front() {
            if self.send_end.is_some_and(|end| log.position(&id).is_none_or(|i| i >= end)) { continue }
            if let Some(event) = log.get(&id) {
                let mut bytes = Message::Events { room: log.room_id(), records: &[] }.encode();
                encode_record(event, &mut bytes);
                return Some(bytes);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, Chat, Content, Draft, Via};
    use crate::fold::fold;
    use crate::key::DeviceKey;
    use std::path::PathBuf;

    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("makepad-create-sync-{name}-{}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn log(&self, name: &str, genesis: &Event) -> Log {
            Log::found(&self.0.join(name), genesis.clone()).unwrap()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn genesis(key: &DeviceKey) -> Event {
        Event::found(key, 2, 0, 0, [0; 16]).unwrap().1
    }

    fn chat(key: &DeviceKey, room: Id, parents: Vec<Id>, text: &str) -> Event {
        let content = Content::Chat(Chat { text: text.into(), ..Chat::default() });
        let draft = Draft {
            kind: content.kind(), kind_version: content.version(), critical: false,
            actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents, content: &content.encode(),
        };
        Event::create(key, room, draft, [0; 16]).unwrap()
    }

    fn events_message(room: Id, events: &[&Event]) -> Vec<u8> {
        let mut bytes = Message::Events { room, records: &[] }.encode();
        for event in events {
            encode_record(event, &mut bytes);
        }
        bytes
    }

    fn random(seed: &mut u64) -> usize {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (*seed >> 32) as usize
    }

    #[test]
    fn capped_heads_keep_the_latest_and_both_logs_converge() {
        let keys = std::array::from_fn::<_, 3, _>(|_| DeviceKey::generate().unwrap());
        let root = genesis(&keys[0]);
        let dir = Temp::new("capped-heads");
        let mut log = dir.log("log", &root);
        let mut peer = dir.log("peer", &root);
        for key in &keys[1..] {
            crate::log::tests::admit(&mut log, &keys[0], key);
        }
        peer.append_all(log.events().to_vec()).unwrap();

        let mut heads = keys.each_ref().map(|key| chat(key, root.room, log.parents_for(&key.public()), "concurrent"));
        heads.sort_unstable_by_key(|event| event.id);
        heads.swap(0, 1); // The oldest arrival is in the middle by id.
        log.append_all(heads.to_vec()).unwrap();
        peer.append_all(heads[..2].to_vec()).unwrap();
        assert_eq!(log.heads().len(), 3, "the three authors have concurrent heads");

        let (mut local, mut remote) = (SyncEngine::new(), SyncEngine::new());
        local.connect(&log);
        remote.connect(&peer);
        let offer = local.poll_send_with_head_limit(&log, 2).unwrap();
        assert_eq!(Message::decode(&offer).unwrap(), Message::Heads { room: root.room, heads: &[heads[1].id, heads[2].id] },
            "HEADS includes both latest arrivals");
        local.on_message(&remote.poll_send(&peer).unwrap(), &mut log).unwrap();
        remote.on_message(&offer, &mut peer).unwrap();

        let retry = remote.poll_send(&peer).unwrap();
        assert_eq!(retry, events_message(root.room, &[&heads[0]]), "the peer resends the omitted head");
        assert!(local.on_message(&retry, &mut log).unwrap().appended.is_empty(), "the duplicate is skipped");
        let missing = local.poll_send(&log).unwrap();
        assert_eq!(remote.on_message(&missing, &mut peer).unwrap().appended, [heads[2].id]);
        assert_eq!(peer.events(), log.events(), "both logs contain exactly the same records");
        assert!(local.poll_send(&log).is_none() && remote.poll_send(&peer).is_none(), "the exchange is complete");
    }

    #[test]
    fn convergence_across_concurrent_appends_and_reconnect() {
        let keys = [DeviceKey::generate().unwrap(), DeviceKey::generate().unwrap()];
        let root = genesis(&keys[0]);
        let absent = chat(&keys[0], root.room, vec![], "never appended");
        for initial_seed in [1, 3, 7, 11] {
            let dir = Temp::new(&format!("convergence-{initial_seed}"));
            let mut seed = initial_seed;
            let mut left = dir.log("left", &root);
            crate::log::tests::admit(&mut left, &keys[0], &keys[1]);
            let mut right = dir.log("right", &root);
            right.append_all(left.events().to_vec()).unwrap();
            let mut logs = [left, right];
            let mut engines = [SyncEngine::new(), SyncEngine::new()];
            let mut expected = HashSet::new();
            for side in 0..2 {
                let event = chat(&keys[side], root.room, logs[side].parents_for(&keys[side].public()), "before connect");
                expected.insert(event.id);
                logs[side].append(event).unwrap();
                engines[side].connect(&logs[side]);
            }

            // One in-flight message per direction. Replacing the link drops its in-flight bytes.
            let mut pipe = [None, None];
            let reconnect_at = 2 + random(&mut seed) % 3;
            let mut live_edit = false;
            let mut deliveries = 0;
            loop {
                for side in 0..2 {
                    if pipe[side].is_none() {
                        pipe[side] = engines[side].poll_send(&logs[side]);
                    }
                }
                let idle = pipe.iter().all(Option::is_none);
                if deliveries == 1 || (idle && !live_edit) {
                    for side in 0..2 {
                        let event = chat(&keys[side], root.room, logs[side].parents_for(&keys[side].public()),
                            if idle { "after catch-up" } else { "during catch-up" });
                        expected.insert(event.id);
                        logs[side].append(event.clone()).unwrap();
                        if idle {
                            engines[side].on_local_append(&absent);
                        }
                        engines[side].on_local_append(&event);
                    }
                    if idle {
                        live_edit = true;
                        continue;
                    }
                }
                if idle { break }
                let mut from = random(&mut seed) % 2;
                if pipe[from].is_none() { from = 1 - from; }
                let to = 1 - from;
                let bytes = pipe[from].take().unwrap();
                if live_edit {
                    assert_eq!(bytes[1], EVENTS, "seed {initial_seed}: live writes produce no Heads");
                }
                let result = engines[to].on_message(&bytes, &mut logs[to]).unwrap();
                assert_eq!(result.missing_parents, 0, "seed {initial_seed}: every delivery is ancestor-closed");
                deliveries += 1;
                assert!(deliveries < 100, "seed {initial_seed}: the exchange must quiesce");
                if deliveries == reconnect_at {
                    engines = [SyncEngine::new(), SyncEngine::new()];
                    pipe = [None, None];
                    for side in 0..2 { engines[side].connect(&logs[side]); }
                }
            }
            assert!(deliveries > reconnect_at && live_edit, "seed {initial_seed}: exercised reconnect and live sync");
            assert_eq!(expected.len(), 6, "seed {initial_seed}: both writers posted in all three phases");
            for log in &logs {
                let ids: HashSet<_> = log.events().iter().filter(|e| e.header.kind == crate::event::kind::CHAT).map(|e| e.id).collect();
                assert_eq!(ids, expected, "seed {initial_seed}: every local write reached both logs");
            }
            let state = fold(logs[0].events());
            assert_eq!(state.chat.len(), expected.len(), "seed {initial_seed}: all events folded");
            assert_eq!(state, fold(logs[1].events()), "seed {initial_seed}: both folds agree");
        }
    }

    #[test]
    fn membership_gates_both_live_and_catch_up_history() {
        use crate::event::Member;

        fn member(log: &mut Log, key: &DeviceKey, op: Member) -> Id {
            let content = Content::Member(op);
            let bytes = content.encode();
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical: true,
                actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents: log.parents_for(&key.public()), content: &bytes };
            let event = Event::create(key, log.room_id(), draft, [1; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            id
        }

        let dir = Temp::new("membership");
        let founder = DeviceKey::generate().unwrap();
        let joiner = DeviceKey::generate().unwrap();
        let (a, b) = (founder.public(), joiner.public());
        let root = genesis(&founder);
        let mut log = dir.log("space", &root);
        let mut live = SyncEngine::new();
        live.connect(&log);
        assert!(!live.serve(&log, &fold(log.events()), &a, &b), "an unknown peer has no sendable history");
        assert_eq!(live.poll_send(&log), Some(Message::Heads { room: root.room, heads: &[] }.encode()), "an unknown peer cannot see even the room's heads");

        let seat = member(&mut log, &founder, Member::Invite { subject: b, role: 0 });
        let state = fold(log.events());
        assert_eq!(state.invitation(&b), Some(seat), "the first accept can name the invite");
        assert!(live.serve(&log, &state, &a, &b), "an invited peer is eligible to receive the history");
        let early = chat(&joiner, log.room_id(), vec![seat], "before accepting");
        assert!(matches!(live.on_message(&events_message(root.room, &[&early]), &mut log), Err(SyncError::Event(_))),
            "the invited key's first chat is refused by the author chain");
        assert!(log.get(&early.id).is_none(), "the refused chat never enters the room");
        let accept = member(&mut log, &joiner, Member::Accept { entry: seat });
        let before = chat(&founder, log.room_id(), log.parents_for(&a), "before leaving");
        log.append(before.clone()).unwrap();
        live.on_message(&Message::Heads { room: root.room, heads: &[accept] }.encode(), &mut log).unwrap();
        let removal = member(&mut log, &joiner, Member::Remove { seat, device: b, last: accept });
        let later = chat(&founder, log.room_id(), log.parents_for(&a), "after leaving");
        log.append(later.clone()).unwrap();
        live.on_local_append(log.get(&removal).unwrap());
        live.on_local_append(&later);
        let state = fold(log.events());
        assert_eq!(state.active_members().collect::<Vec<_>>(), [a], "the founder is alone after the leave");
        assert_eq!(state.seat_of(&b), Some(seat), "the former member keeps its historical seat");
        assert!(live.serve(&log, &state, &a, &b), "the final exchange can deliver the leave");
        assert_eq!(live.poll_send(&log), Some(events_message(root.room, &[&before])), "an in-flight difference delivers the earlier edit");
        assert_eq!(live.poll_send(&log), Some(events_message(root.room, &[log.get(&removal).unwrap()])), "live delivery includes the removal");
        assert!(live.poll_send(&log).is_none(), "live delivery withholds the later edit");

        let mut catch_up = SyncEngine::new();
        catch_up.connect(&log);
        assert!(catch_up.serve(&log, &state, &b, &a), "a leaver can deliver its own removal on a later link");
        assert_eq!(catch_up.poll_send(&log), Some(Message::Heads { room: root.room, heads: &[removal] }.encode()), "reconnect advertises only heads of the allowed prefix");
        catch_up.on_message(&Message::Heads { room: root.room, heads: &[accept, later.id] }.encode(), &mut log).unwrap();
        assert_eq!(catch_up.poll_send(&log), Some(events_message(root.room, &[&before, log.get(&removal).unwrap()])),
            "a post-removal peer head confers no knowledge of the prefix");
        assert!(catch_up.poll_send(&log).is_none(), "the catch-up scan ends at the removal");

        let c = DeviceKey::generate().unwrap().public();
        let extra = member(&mut log, &founder, Member::Invite { subject: c, role: 0 });
        let state = fold(log.events());
        assert_eq!(state.invitation(&c), Some(extra),
            "after a member leaves, the founder's new invite offers a seat to the fresh key");
    }

    #[test]
    fn orphan_requests_heads_and_retries_in_parent_order() {
        let dir = Temp::new("orphan");
        let key = DeviceKey::generate().unwrap();
        let root = genesis(&key);
        let mut log = dir.log("log", &root);
        let mut engine = SyncEngine::new();
        let parent = chat(&key, root.room, vec![root.id], "parent");
        let child = chat(&key, root.room, vec![parent.id], "child");
        let received = engine.on_message(&events_message(root.room, &[&child]), &mut log).unwrap();
        assert_eq!(received.missing_parents, 1, "one record lacks a parent");
        assert!(log.events().len() == 1 && received.appended.is_empty(), "an orphan is never stored");
        assert_eq!(engine.poll_send(&log), Some(Message::Heads { room: root.room, heads: &[root.id] }.encode()), "the gap requests a fresh exchange");
        let received = engine.on_message(&events_message(root.room, &[&parent, &parent, &child]), &mut log).unwrap();
        assert_eq!(received.appended, [parent.id, child.id], "an earlier record in the batch satisfies a parent");
        assert_eq!(received.missing_parents, 0, "the retry closes the gap");
    }

    #[test]
    fn malformed_messages_leave_even_the_valid_prefix_unapplied() {
        let dir = Temp::new("malformed");
        let key = DeviceKey::generate().unwrap();
        let root = genesis(&key);
        let mut log = dir.log("log", &root);
        let mut engine = SyncEngine::new();
        let initial = std::fs::read(dir.0.join("log")).unwrap();
        let event = chat(&key, root.room, vec![root.id], "valid prefix");
        let owner = DeviceKey::generate().unwrap();
        let other = genesis(&owner);
        let mut elsewhere = dir.log("elsewhere", &other);
        let foreign = chat(&owner, other.room, vec![other.id], "other room");
        elsewhere.append(foreign.clone()).unwrap();
        let mut sender = SyncEngine::new();
        sender.on_message(&Message::Heads { room: other.room, heads: &[] }.encode(), &mut elsewhere).unwrap();
        let message = sender.poll_send(&elsewhere).unwrap();
        assert!(matches!(engine.on_message(&message, &mut log), Err(SyncError::Malformed(_))),
            "a complete sync from another room is refused");
        assert_eq!(log.events(), std::slice::from_ref(&root), "no foreign record was stored");
        let foreign_batch = events_message(root.room, &[&event, &foreign]);
        let fork = chat(&key, root.room, vec![root.id], "fork");
        let fork_batch = events_message(root.room, &[&event, &fork]);
        assert!(matches!(engine.on_message(&fork_batch, &mut log), Err(SyncError::Event(_))),
            "a broken author chain has the same refusal as an invalid signature");
        let mut bad_sig = events_message(root.room, &[&event, &event]);
        *bad_sig.last_mut().unwrap() ^= 1;
        let mut bad_len = events_message(root.room, &[&event]);
        bad_len.extend_from_slice(&u32::MAX.to_le_bytes());
        let truncated = &bad_sig[..bad_sig.len() - 1];
        let mut unknown = Message::Heads { room: root.room, heads: &[] }.encode();
        unknown[1] = 0xff;
        let mut partial = Message::Heads { room: root.room, heads: &[] }.encode();
        partial.push(0);
        for (property, bytes) in [
            ("short envelope", &[VERSION][..]), ("wrong version", &[VERSION + 1, HEADS]),
            ("unknown kind", unknown.as_slice()), ("partial head", partial.as_slice()),
            ("foreign room", &foreign_batch), ("forked chain", &fork_batch), ("short record", truncated), ("length beyond message", &bad_len), ("invalid signature", &bad_sig),
        ] {
            assert!(engine.on_message(bytes, &mut log).is_err(), "{property} returns an error");
            assert!(log.events().len() == 1, "{property} appends no prefix");
        }
        assert_eq!(std::fs::read(dir.0.join("log")).unwrap(), initial, "malformed input never reached disk");
    }

    #[test]
    fn duplicate_full_body_preserves_local_redaction() {
        let dir = Temp::new("redaction");
        let key = DeviceKey::generate().unwrap();
        let root = genesis(&key);
        let mut log = dir.log("redacted", &root);
        let mut peer = dir.log("full", &root);
        let full = chat(&key, root.room, vec![root.id], "erase this");
        let erased = Event::from_parts(root.room, full.header_bytes.clone(), vec![], full.sig.clone()).unwrap();
        log.append(erased).unwrap();
        peer.append(full.clone()).unwrap();
        let mut engine = SyncEngine::new();
        let mut bytes = Message::Events { room: log.room_id(), records: &[] }.encode();
        bytes.extend_from_slice(&std::fs::read(dir.0.join("full")).unwrap());
        let received = engine.on_message(&bytes, &mut log).unwrap();
        assert!(received.appended.is_empty(), "the full copy is a duplicate");
        drop(log);
        let reopened = Log::open(&dir.0.join("redacted")).unwrap().0;
        assert!(reopened.get(&full.id).unwrap().redacted(), "the durable local copy stays redacted");
        assert!(engine.poll_send(&reopened).is_none(), "a duplicate alone needs no confirmation");
    }

    #[test]
    fn difference_splits_at_record_boundaries_and_skips_known_ancestors() {
        let dir = Temp::new("split");
        let key = DeviceKey::generate().unwrap();
        let root = genesis(&key);
        let (mut log, mut peer) = (dir.log("sender", &root), dir.log("receiver", &root));
        for text in ["common root", "common head"] {
            let event = chat(&key, root.room, log.heads(), text);
            log.append(event.clone()).unwrap();
            peer.append(event).unwrap();
        }
        let mut recv = SyncEngine::new();
        recv.connect(&peer);
        let mut heads = recv.poll_send(&peer).unwrap();
        heads.extend_from_slice(&[0xff; 32]); // A peer branch the sender hasn't seen.
        let common_bytes = std::fs::metadata(dir.0.join("sender")).unwrap().len() as usize;
        let mut expected = Vec::new();
        let text = "x".repeat(BATCH_BYTES / 3);
        for _ in 0..5 {
            let event = chat(&key, root.room, log.heads(), &text);
            expected.push(event.id);
            log.append(event).unwrap();
        }
        let mut send = SyncEngine::new();
        send.on_message(&heads, &mut log).unwrap();
        let mut sizes = Vec::new();
        let mut appended = Vec::new();
        while let Some(bytes) = send.poll_send(&log) {
            sizes.push(bytes.len());
            let received = recv.on_message(&bytes, &mut peer).unwrap();
            assert_eq!(received.missing_parents, 0, "each split begins after all its parents");
            appended.extend(received.appended);
            assert_eq!(send.peer_heads_answered(), appended.len() == expected.len(), "the reply is complete only after its last batch");
        }
        assert!(sizes.len() > 1 && sizes.iter().all(|n| *n <= BATCH_BYTES), "ordinary records respect the split target");
        assert_eq!(appended, expected, "all missing records arrive in log order");
        let disk = std::fs::read(dir.0.join("sender")).unwrap();
        let sent_record_bytes: usize = sizes.iter().map(|n| n - HEADER_BYTES).sum();
        assert_eq!(sent_record_bytes, disk.len() - common_bytes, "the common head and its ancestor were omitted");
        assert_eq!(std::fs::read(dir.0.join("receiver")).unwrap(), disk, "relayed records are byte exact");
        assert!(recv.poll_send(&peer).is_none(), "successful catch-up batches need no reply");

        let large = chat(&key, root.room, log.heads(), &"y".repeat(BATCH_BYTES));
        log.append(large.clone()).unwrap();
        recv.connect(&peer);
        send.on_message(&recv.poll_send(&peer).unwrap(), &mut log).unwrap();
        let bytes = send.poll_send(&log).unwrap();
        assert_eq!(recv.on_message(&bytes, &mut peer).unwrap().appended, [large.id], "one oversized record still arrives");
        assert!(send.poll_send(&log).is_none(), "the oversized record is sent once");
    }
}
