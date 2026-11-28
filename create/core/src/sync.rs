//! Transport-independent sync for two ancestor-closed copies of one town. Pass one complete message per call.
//! The heads exchange alone converges any two devices. The live push is an optimisation.
//! Version 3 starts with the version, kind and 32-byte town id. The rest of a Heads or HeadsDone message is consecutive
//! 32-byte ids. The rest of an Events message is consecutive little-endian log records.

use crate::event::{Event, FormatError, Id};
use crate::log::{encode_record, record, Log};
use crate::fold::State;
use crate::key::PublicKey;
use crate::share::{Message, HEADER_BYTES};
use makepad_network::digest::sha256_hash;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::{cmp::Reverse, fmt, io, ops::Range, sync::{Arc, Weak}};

pub(crate) const VERSION: u8 = 3;
pub(crate) const HEADS: u8 = 1;
pub(crate) const EVENTS: u8 = 2;
// The previous u32 frame length included the kind byte. The 16 MiB cap comes with L3t.
pub(crate) const MAX_MESSAGE: usize = u32::MAX as usize - 1;
// Split between records. A single larger record gets its own message.
pub(crate) const BATCH_BYTES: usize = 64 * 1024;
const MAX_HEADS: usize = (MAX_MESSAGE - HEADER_BYTES) / 32;

#[derive(Debug)]
pub enum SyncError {
    Malformed(&'static str),
    Event(FormatError),
    Append(io::Error),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
    /// A completed difference whose advertised frontier is present in this log.
    pub caught_up: bool,
}

struct Difference {
    known: HashSet<Id>,
    remaining: Range<usize>,
}

struct Frontiers {
    end: usize,
    tips: usize,
    // Nearest sample first, latest arrival first within a sample.
    heads: Vec<(usize, Id)>,
}

impl Frontiers {
    fn sample(log: &Log, end: usize) -> Self {
        let mut positions = Vec::new();
        let mut distance = 0usize;
        while distance < end {
            positions.push(end - distance);
            if end - distance == 1 { break }
            let step = if distance < 16 { 1 } else { distance / 4 };
            distance += step.min(end - distance - 1);
        }

        let mut heads = HashMap::new();
        let mut ranked = Vec::new();
        let mut sampled = 0;
        for (index, event) in log.events()[..end].iter().enumerate() {
            for parent in &event.header.parents {
                if let Some(arrival) = heads.remove(parent) {
                    // A retiring head was visible at the latest sample iff it arrived before
                    // that prefix ended. Record it there once, however many samples it spanned.
                    if arrival < sampled { ranked.push((sampled, arrival)); }
                }
            }
            heads.insert(event.id, index);
            if positions.last() == Some(&(index + 1)) {
                sampled = index + 1;
                positions.pop();
            }
        }
        let tips = heads.len();
        ranked.extend(heads.into_values().map(|arrival| (end, arrival)));
        ranked.sort_unstable_by_key(|&rank| Reverse(rank));
        let heads = ranked.into_iter().map(|(_, i)| (i, log.events()[i].id)).collect();
        Self { end, tips, heads }
    }

    fn matches(&self, log: &Log, end: usize) -> bool {
        // Equal lengths can describe different replicas or arrival orders. All heads of
        // each sampled prefix must still occur at the recorded positions. By ancestor
        // closure those heads account for the entire prefix, including unsampled events.
        self.end == end && self.heads.iter().all(|&(i, id)| log.events()[i].id == id)
    }

    fn message(&self, town: Id, limit: usize) -> Vec<u8> {
        let mut heads: Vec<_> = self.heads.iter().take(limit).map(|&(_, id)| id).collect();
        heads[..self.tips.min(limit)].sort_unstable();
        Message::Heads { town, heads: &heads }.encode()
    }
}

// Carriers on the model thread share the last sample for a town. Its storage lives
// with the engines that used it, including when a town or a whole home is closed.
thread_local! {
    static FRONTIERS: RefCell<HashMap<Id, Weak<Frontiers>>> = RefCell::new(HashMap::new());
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
    // On this connection a rescan cannot supply a readable version of these headers or their descendants.
    unreadable: HashSet<Id>,
    difference: Option<Difference>,
    live: VecDeque<Id>,
    send_end: Option<usize>,
    final_evidence: Vec<Id>,
    shown_final: Vec<Id>,
    history_base: HistoryBase,
    frontiers: Option<Arc<Frontiers>>,
}

/// Exclusive arrival position up to which these devices may sync, including the first counted
/// removal or the second sibling proving a fork. None includes the whole log.
pub fn send_end(log: &Log, state: &State, me: &PublicKey, peer: &PublicKey) -> Option<usize> {
    if !state.has_standing(me) || !state.has_standing(peer) {
        return Some(0);
    }
    let eligible = |key| state.active_member(key) || state.invitation(key).is_some();
    match state.final_boundary(log, me, peer) {
        Some(boundary) => Some(boundary.at + 1),
        None if eligible(me) && eligible(peer) => None,
        None => Some(0),
    }
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
        let (end, evidence) = if !state.has_standing(me) || !state.has_standing(peer) {
            (Some(0), Vec::new())
        } else if let Some(boundary) = state.final_boundary(log, me, peer) {
            (Some(boundary.at + 1), boundary.evidence)
        } else if (state.active_member(me) || state.invitation(me).is_some())
            && (state.active_member(peer) || state.invitation(peer).is_some())
        {
            (None, Vec::new())
        } else {
            (Some(0), Vec::new())
        };
        // A late membership decision can make previously withheld records eligible.
        // A fresh scan can resend duplicates, which the receiver already verifies and skips.
        let visible = end.unwrap_or(log.events().len());
        if self.history_base != HistoryBase::None && self.send_end.is_some_and(|old| old < visible) {
            self.difference = Some(Difference { known: HashSet::new(), remaining: 0..visible });
        }
        self.send_end = end;
        self.final_evidence = evidence;
        end != Some(0)
    }

    /// An established carrier is retiring this town. No further HEADS exchange is
    /// required. If the peer has not sent heads yet, start at genesis so its final
    /// removal can still arrive with its parents. Otherwise queue every proof event:
    /// the removal, or both fork siblings.
    pub fn finish_prefix(&mut self, log: &Log) -> bool {
        if self.final_evidence.is_empty()
            || self.send_end.and_then(|end| end.checked_sub(1)).and_then(|i| log.events().get(i)).is_none()
        {
            return false;
        }
        if self.history_base == HistoryBase::None {
            self.start_difference(&[], log, HistoryBase::PeerHeads);
        } else {
            for &id in &self.final_evidence {
                if !self.live.contains(&id) {
                    self.live.push_back(id);
                }
            }
        }
        true
    }

    pub fn shown_final(&self) -> &[Id] { &self.shown_final }
    pub fn final_evidence(&self) -> &[Id] { &self.final_evidence }

    /// Queue this link's evidence when it first serves a final-bounded prefix. Its last
    /// event is a head. Tips alone carry the final evidence and keep this frame small.
    pub fn take_final_heads(&mut self, log: &Log) -> Option<Vec<u8>> {
        if self.shown_final == self.final_evidence { return None }
        self.shown_final.clone_from(&self.final_evidence);
        if self.shown_final.is_empty() { return None }
        self.heads_pending = false;
        Some(self.tips_message(log, MAX_HEADS, None))
    }

    pub fn on_message(&mut self, bytes: &[u8], log: &mut Log) -> Result<Received, SyncError> {
        match Message::decode(bytes)? {
            Some(message) => self.receive(message, log),
            None => Ok(Received::default()),
        }
    }

    pub fn receive(&mut self, message: Message<'_>, log: &mut Log) -> Result<Received, SyncError> {
        match message {
            Message::Heads { town, heads } if town == log.town_id() => {
                self.start_difference(heads, log, HistoryBase::PeerHeads);
                Ok(Received::default())
            }
            Message::HeadsDone { town, heads } if town == log.town_id() => {
                Ok(Received { caught_up: heads.iter().all(|id| log.get(id).is_some()), ..Received::default() })
            }
            Message::Events { town, records } if town == log.town_id() => self.receive_events(records, log),
            Message::Heads { .. } | Message::HeadsDone { .. } | Message::Events { .. } => Err(SyncError::Malformed("wrong sync town")),
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
        // Stage in wire order. Every record admitted to the log must verify.
        let mut accepted = Vec::new();
        let mut ids = HashSet::new();
        let mut unreadable = HashSet::new();
        let mut received = Received::default();
        while !payload.is_empty() {
            let ((header, body, sig), used) = record(payload).ok_or(SyncError::Malformed("incomplete record"))?;
            payload = &payload[used..];
            let id = sha256_hash(header);
            let stored = log.get(&id);
            if let Some(event) = stored.filter(|event| event.sig == sig && event.body == body) {
                if event.header.kind == crate::event::kind::MEMBER {
                    received.delivered_membership.push(event.id);
                }
                continue;
            }
            let event = match Event::from_parts(log.town_id(), header.to_vec(), body.to_vec(), sig.to_vec()) {
                Ok(event) => event,
                Err(FormatError::NewerFormat(_)) => { unreadable.insert(id); continue }
                Err(e) => return Err(SyncError::Event(e)),
            };
            if event.header.kind == crate::event::kind::MEMBER {
                received.delivered_membership.push(event.id);
            }
            if stored.is_some() || ids.contains(&event.id) {
                continue;
            }
            let mut missing = event.header.parents.iter().filter(|p| !ids.contains(*p) && log.get(p).is_none()).peekable();
            if missing.peek().is_some() {
                if missing.any(|p| self.unreadable.contains(p) || unreadable.contains(p)) {
                    unreadable.insert(event.id);
                } else {
                    received.missing_parents += 1;
                }
            } else {
                ids.insert(event.id);
                received.appended.push(event.id);
                accepted.push(event);
            }
        }
        log.check_batch(&accepted).map_err(SyncError::Event)?;
        log.append_all(accepted).map_err(SyncError::Append)?;
        self.unreadable.extend(unreadable);
        self.heads_pending |= received.missing_parents != 0;
        Ok(received)
    }

    /// Notify in local append order. Appends before the first peer Heads are in its scan.
    pub fn on_append(&mut self, event: &Event) {
        if self.history_base != HistoryBase::None {
            self.live.push_back(event.id);
        }
    }

    pub fn poll_send(&mut self, log: &Log) -> Option<Vec<u8>> {
        self.poll_send_with_head_limit(log, MAX_HEADS)
    }

    pub fn needs_heads(&self) -> bool { self.heads_pending }

    fn tips_message(&self, log: &Log, max_heads: usize, completed_end: Option<usize>) -> Vec<u8> {
        let end = self.send_end.unwrap_or(usize::MAX).min(completed_end.unwrap_or(usize::MAX));
        let mut heads = log.heads_before(end);
        if heads.len() > max_heads {
            // Omitted heads can only make the peer resend records we already have.
            heads.select_nth_unstable_by_key(max_heads, |id| std::cmp::Reverse(log.position(id)));
            heads.truncate(max_heads);
            heads.sort_unstable();
        }
        let town = log.town_id();
        if completed_end.is_some() { Message::HeadsDone { town, heads: &heads }.encode() }
        else { Message::Heads { town, heads: &heads }.encode() }
    }

    fn heads_message(&mut self, log: &Log, max_heads: usize) -> Vec<u8> {
        let end = self.send_end.unwrap_or(log.events().len()).min(log.events().len());
        let town = log.town_id();
        let frontiers = FRONTIERS.with_borrow_mut(|towns| {
            if let Some(prior) = towns.get(&town).and_then(Weak::upgrade) {
                if prior.matches(log, end) { return prior }
            }
            let sampled = Arc::new(Frontiers::sample(log, end));
            towns.retain(|_, cached| cached.strong_count() != 0);
            towns.insert(town, Arc::downgrade(&sampled));
            sampled
        });
        let message = frontiers.message(town, max_heads);
        self.frontiers = Some(frontiers);
        message
    }

    fn poll_send_with_head_limit(&mut self, log: &Log, max_heads: usize) -> Option<Vec<u8>> {
        if std::mem::take(&mut self.heads_pending) { Some(self.heads_message(log, max_heads)) }
        else { self.poll_reply(log) }
    }

    /// Diff replies and live pushes remain writable while another peer supplies this town's history.
    pub fn poll_reply(&mut self, log: &Log) -> Option<Vec<u8>> {
        if let Some(diff) = &mut self.difference {
            let mut batch = Message::Events { town: log.town_id(), records: &[] }.encode();
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
            if batch.len() > HEADER_BYTES {
                return Some(batch);
            }
            // Later appends are in the live FIFO. Only claim the frontier covered by this scan.
            let end = diff.remaining.end;
            self.difference = None;
            if self.history_base == HistoryBase::PeerHeads {
                return Some(self.tips_message(log, MAX_HEADS, Some(end)));
            }
        }

        while let Some(id) = self.live.pop_front() {
            if self.send_end.is_some_and(|end| log.position(&id).is_none_or(|i| i >= end)) { continue }
            if let Some(event) = log.get(&id) {
                let mut bytes = Message::Events { town: log.town_id(), records: &[] }.encode();
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

    fn chat(key: &DeviceKey, town: Id, parents: Vec<Id>, text: &str) -> Event {
        let content = Content::Chat(Chat { text: text.into(), ..Chat::default() });
        let draft = Draft {
            kind: content.kind(), kind_version: content.version(), critical: false,
            actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents, content: &content.encode(),
        };
        Event::create(key, town, draft, [0; 16]).unwrap()
    }

    fn events_message(town: Id, events: &[&Event]) -> Vec<u8> {
        let mut bytes = Message::Events { town, records: &[] }.encode();
        for event in events {
            encode_record(event, &mut bytes);
        }
        bytes
    }

    fn older_checkpoint(state: &State, events: &[Event], learned: Id, skipped_tag: bool) -> Vec<u8> {
        let mut older = state.clone();
        if skipped_tag { older.partial.push(learned); } else { older.unknown.push(learned); }
        older.checkpoint(events)
    }

    #[test]
    fn future_records_relay_and_checkpoints_follow_what_the_fold_read() {
        use crate::event::{kind, FieldWriter};
        use crate::key::Domain;

        let dir = Temp::new("future-records");
        let ana = DeviceKey::generate().unwrap();
        let root = genesis(&ana);
        let town = root.town;
        let mut ben = dir.log("ben", &root);
        let mut receiver = SyncEngine::new();
        receiver.connect(&ben);
        for bytes in [&[3, 250][..], &[4, HEADS]] {
            assert!(receiver.on_message(bytes, &mut ben).expect("an unknown sync kind or version is ignored").appended.is_empty(),
                "an ignored frame leaves the log alone");
        }
        assert!(matches!(Message::decode(&receiver.poll_send(&ben).expect("the connection still sends its opening heads")), Ok(Some(Message::Heads { .. }))),
            "ignored input preserves the live exchange");

        let mut parent = root.id;
        let mut records = Vec::new();
        for (kind, version, flags, extension) in [
            (499u16, 1, 0, &[][..]), (kind::CHAT, 2, 0, &[]),
            (kind::CHAT, 1, 0x81, &[2, 0, 0, 0, 0xdd, 0xee]),
            (kind::ENTITY, 4, 8, &[3, 0, 0, 0, 0xff, 0, 0xab]),
        ] {
            let base = chat(&ana, town, vec![parent], "Ana");
            let mut wire = base.header_bytes;
            wire[1..3].copy_from_slice(&kind.to_le_bytes());
            wire[3] = version;
            wire[4] = flags;
            wire.extend_from_slice(extension);
            let sig = ana.sign(Domain::Event, &[town.as_slice(), &wire].concat()).unwrap();
            let event = Event::from_parts(town, wire.clone(), base.body, sig).expect("the future record verifies");
            assert_eq!(event.header.encode(), wire, "kind {kind} v{version} preserves every header byte");
            parent = event.id;
            records.push(event);
        }
        let bytes = events_message(town, &records.iter().collect::<Vec<_>>());
        let received = receiver.on_message(&bytes, &mut ben).expect("the whole future batch is accepted");
        let ids: Vec<_> = records.iter().map(|e| e.id).collect();
        assert_eq!(received.appended, ids, "all four verified records were stored");
        drop(ben);
        let (mut ben, problems) = Log::open(&dir.0.join("ben")).expect("the signed log reopens");
        assert!(problems.is_empty(), "reopening reports no unreadable record");
        for event in &records {
            assert_eq!(ben.get(&event.id), Some(event), "reopening preserves body, signed header and signature for each future record");
        }
        let mut relay = SyncEngine::new();
        relay.receive(Message::Heads { town, heads: &[root.id] }, &mut ben).unwrap();
        assert_eq!(relay.poll_send(&ben), Some(bytes), "the reopened log relays all four original frames byte for byte");

        let mut extended = Content::Chat(Chat { text: "Ben".into(), ..Chat::default() }).encode();
        extended.extend(FieldWriter::default().bytes(201, b"future").finish());
        let tagged = Event::create(&ana, town, Draft { kind: kind::CHAT, kind_version: 1, critical: false,
            actor: Actor::Human, via: Via::Ui, wall_ms: 1, parents: vec![parent], content: &extended }, [0; 16]).unwrap();
        let full = chat(&ana, town, vec![tagged.id], "Kit");
        ben.append_all(vec![tagged.clone(), full.clone()]).unwrap();
        let state = fold(ben.events());
        assert_eq!(state.unknown, [ids[0], ids[1], ids[3]], "unknown kinds, versions and opaque shapes share the unknown class");
        assert_eq!(state.unknown_critical, [ids[2]], "the opaque critical header retains its class");
        assert_eq!(state.partial, [tagged.id], "a decoded event with a skipped tag is partial");
        assert_eq!(state.chat[0].chat.text, "Ben", "the fold applies the fields it could read");
        assert_eq!(State::restore(ben.events(), &state.checkpoint(ben.events())), Some(state.clone()),
            "an unchanged classification restores all three lists and projections");
        for (skipped, property) in [(false, "newly known record"), (true, "newly understood tag")] {
            assert!(State::restore(ben.events(), &older_checkpoint(&state, ben.events(), full.id, skipped)).is_none(),
                "a {property} invalidates the older checkpoint");
        }

        let mut kit = dir.log("kit", &root);
        kit.append_all(ben.events()[1..].to_vec()).unwrap();
        // A future writer's signed record fills one ordinary Events frame. Children are in the
        // next frame on every scan, including any repair the receiver requests.
        let mut future = Event::create(&ana, town, Draft { kind: kind::CHAT, kind_version: 1, critical: false,
            actor: Actor::Human, via: Via::Ui, wall_ms: 2, parents: vec![full.id], content: &vec![0; BATCH_BYTES] }, [1; 16]).unwrap();
        future.header_bytes[0] = 2;
        future.id = sha256_hash(&future.header_bytes);
        future.sig = ana.sign(Domain::Event, &[town.as_slice(), &future.header_bytes].concat()).unwrap();
        let child = chat(&ana, town, vec![future.id], "Ben");
        let grandchild = chat(&ana, town, vec![child.id], "Kit");
        let independent = chat(&ana, town, vec![full.id], "Ana");
        ben.append_all(vec![future.clone(), child.clone(), grandchild.clone(), independent.clone()]).unwrap();

        let mut sender = SyncEngine::new();
        receiver.connect(&kit);
        let mut rounds = 0;
        let mut future_batches = 0;
        for _ in 0..2 {
            let Some(request) = receiver.poll_send(&kit) else { break };
            assert!(matches!(Message::decode(&request), Ok(Some(Message::Heads { .. }))), "each scan starts with a Heads request");
            rounds += 1;
            sender.on_message(&request, &mut ben).unwrap();
            while let Some(frame) = sender.poll_send(&ben) {
                if let Some(Message::Events { records, .. }) = Message::decode(&frame).unwrap() {
                    let ((header, _, _), _) = record(records).expect("Events contains a record");
                    if header == future.header_bytes { future_batches += 1; }
                }
                receiver.on_message(&frame, &mut kit).expect("an unreadable format preserves the exchange");
            }
        }
        assert_eq!((rounds, future_batches), (1, 1), "a FORMAT 2 record and later descendants end without a repeated Heads exchange");
        assert!(receiver.poll_send(&kit).is_none(), "the receiver is quiescent after the difference");
        assert!(kit.get(&future.id).is_none() && kit.get(&child.id).is_none() && kit.get(&grandchild.id).is_none(),
            "unreadable ancestry stays outside the log");
        assert_eq!(kit.get(&independent.id), Some(&independent), "independent history still arrives");
        receiver.connect(&kit);
        let retry = receiver.on_message(&events_message(town, &[&grandchild]), &mut kit).unwrap();
        assert_eq!(retry.missing_parents, 1, "a fresh connection can request the missing parent again");
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

        let mut heads = keys.each_ref().map(|key| chat(key, root.town, log.parents_for(&key.public()), "concurrent"));
        heads.sort_unstable_by_key(|event| event.id);
        heads.swap(0, 1); // The oldest arrival is in the middle by id.
        log.append_all(heads.to_vec()).unwrap();
        peer.append_all(heads[..2].to_vec()).unwrap();
        assert_eq!(log.heads().len(), 3, "the three authors have concurrent heads");

        let (mut local, mut remote) = (SyncEngine::new(), SyncEngine::new());
        local.connect(&log);
        remote.connect(&peer);
        let offer = local.poll_send_with_head_limit(&log, 2).unwrap();
        assert_eq!(Message::decode(&offer).unwrap(), Some(Message::Heads { town: root.town, heads: &[heads[1].id, heads[2].id] }),
            "HEADS includes both latest arrivals");
        // Same town, length, tips and last record, but a different order before that record.
        let mut reordered = dir.log("reordered", &root);
        reordered.append_all(log.events()[1..log.events().len() - 3].to_vec()).unwrap();
        reordered.append_all(vec![heads[1].clone(), heads[0].clone(), heads[2].clone()]).unwrap();
        let mut third = SyncEngine::new();
        third.connect(&reordered);
        let offer3 = third.poll_send_with_head_limit(&reordered, 2).unwrap();
        assert_eq!(Message::decode(&offer3).unwrap(), Some(Message::Heads { town: root.town, heads: &[heads[0].id, heads[2].id] }),
            "each replica samples its own arrival order");
        local.on_message(&remote.poll_send(&peer).unwrap(), &mut log).unwrap();
        remote.on_message(&offer, &mut peer).unwrap();

        let retry = remote.poll_send(&peer).unwrap();
        assert_eq!(retry, events_message(root.town, &[&heads[0]]), "the peer resends the omitted head");
        assert!(local.on_message(&retry, &mut log).unwrap().appended.is_empty(), "the duplicate is skipped");
        let missing = local.poll_send(&log).unwrap();
        assert_eq!(remote.on_message(&missing, &mut peer).unwrap().appended, [heads[2].id]);
        assert_eq!(peer.events(), log.events(), "both logs contain exactly the same records");
        let done = remote.poll_send(&peer).unwrap();
        assert!(local.on_message(&done, &mut log).unwrap().caught_up, "the replied-to prefix is present");
        let done = local.poll_send(&log).unwrap();
        assert!(remote.on_message(&done, &mut peer).unwrap().caught_up);
        assert!(local.poll_send(&log).is_none() && remote.poll_send(&peer).is_none(), "completion causes no reply");
    }

    #[test]
    fn convergence_across_concurrent_appends_and_reconnect() {
        let keys = [DeviceKey::generate().unwrap(), DeviceKey::generate().unwrap()];
        let root = genesis(&keys[0]);
        let absent = chat(&keys[0], root.town, vec![], "never appended");
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
                let event = chat(&keys[side], root.town, logs[side].parents_for(&keys[side].public()), "before connect");
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
                        let event = chat(&keys[side], root.town, logs[side].parents_for(&keys[side].public()),
                            if idle { "after catch-up" } else { "during catch-up" });
                        expected.insert(event.id);
                        logs[side].append(event.clone()).unwrap();
                        if idle {
                            engines[side].on_append(&absent);
                        }
                        engines[side].on_append(&event);
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
    fn one_heads_exchange_bounds_resends_after_either_or_both_replicas_write() {
        fn write(log: &mut Log, key: &DeviceKey, count: usize) {
            let mut parents = log.parents_for(&key.public());
            let mut events = Vec::new();
            for _ in 0..count {
                let event = chat(key, log.town_id(), parents, "a line");
                parents = vec![event.id];
                events.push(event);
            }
            log.append_all(events).unwrap();
        }

        let ana = DeviceKey::generate().unwrap();
        let ben = DeviceKey::generate().unwrap();
        let root = genesis(&ana);
        for (mine, theirs) in [(0, 21), (27, 21)] {
            let dir = Temp::new("apart");
            let mut left = dir.log("ana", &root);
            let mut right = dir.log("ben", &root);
            crate::log::tests::admit(&mut left, &ana, &ben);
            write(&mut left, &ana, 32);
            right.append_all(left.events()[1..].to_vec()).unwrap();
            write(&mut left, &ana, mine);
            write(&mut right, &ben, theirs);
            let mut a = SyncEngine::new();
            let mut b = SyncEngine::new();
            a.connect(&left);
            b.connect(&right);
            let left_heads = a.poll_send(&left).unwrap();
            let right_heads = b.poll_send(&right).unwrap();
            a.on_message(&right_heads, &mut left).unwrap();
            b.on_message(&left_heads, &mut right).unwrap();

            fn reply(sender: &mut SyncEngine, from: &Log, receiver: &mut SyncEngine, to: &mut Log,
                new: usize, away: usize) {
                let (mut sent, mut appended) = (0, 0);
                loop {
                    let bytes = sender.poll_send(from).expect("each opening Heads gets a complete reply");
                    match Message::decode(&bytes).unwrap() {
                        Some(Message::Events { mut records, .. }) => {
                            while !records.is_empty() {
                                let (_, used) = record(records).unwrap();
                                records = &records[used..];
                                sent += 1;
                            }
                        }
                        Some(Message::HeadsDone { heads, .. }) => {
                            assert_eq!(heads.len(), 1, "completion advertises only the tip of the scanned prefix");
                            assert!(receiver.on_message(&bytes, to).unwrap().caught_up);
                            break;
                        }
                        _ => panic!("a difference needs no further round of Heads"),
                    }
                    let got = receiver.on_message(&bytes, to).unwrap();
                    assert_eq!(got.missing_parents, 0);
                    appended += got.appended.len();
                }
                assert_eq!(appended, new, "every local write reached the peer");
                let overshoot = sent - appended;
                assert!(overshoot == 0 || 4 * overshoot < away, "re-sent {overshoot} shared records after {away} peer writes");
                assert!(sender.poll_send(from).is_none(), "one round reached quiescence");
            }
            reply(&mut a, &left, &mut b, &mut right, mine, theirs);
            reply(&mut b, &right, &mut a, &mut left, theirs, mine);
            assert_eq!(left.events().len(), right.events().len());
            assert_eq!(left.heads(), right.heads(), "both directions converged");
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
            let event = Event::create(key, log.town_id(), draft, [1; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            id
        }

        let dir = Temp::new("membership");
        let founder = DeviceKey::generate().unwrap();
        let joiner = DeviceKey::generate().unwrap();
        let (a, b) = (founder.public(), joiner.public());
        let root = genesis(&founder);
        let mut log = dir.log("town", &root);
        let mut live = SyncEngine::new();
        live.connect(&log);
        assert!(!live.serve(&log, &fold(log.events()), &a, &b), "an unknown peer has no sendable history");
        assert_eq!(live.poll_send(&log), Some(Message::Heads { town: root.town, heads: &[] }.encode()), "an unknown peer cannot see even the town's heads");

        let seat = member(&mut log, &founder, Member::Invite { subject: b, role: 0 });
        let state = fold(log.events());
        assert_eq!(state.invitation(&b), Some(seat), "the first accept can name the invite");
        assert!(live.serve(&log, &state, &a, &b), "an invited peer is eligible to receive the history");
        let early = chat(&joiner, log.town_id(), vec![seat], "before accepting");
        assert!(matches!(live.on_message(&events_message(root.town, &[&early]), &mut log), Err(SyncError::Event(_))),
            "the invited key's first chat is refused by the author chain");
        assert!(log.get(&early.id).is_none(), "the refused chat never enters the town");
        let accept = member(&mut log, &joiner, Member::Accept { entry: seat });
        let before = chat(&founder, log.town_id(), log.parents_for(&a), "before leaving");
        log.append(before.clone()).unwrap();
        live.on_message(&Message::Heads { town: root.town, heads: &[accept] }.encode(), &mut log).unwrap();
        let removal = member(&mut log, &joiner, Member::Remove { seat, device: b, last: accept });
        let later = chat(&founder, log.town_id(), log.parents_for(&a), "after leaving");
        log.append(later.clone()).unwrap();
        live.on_append(log.get(&removal).unwrap());
        live.on_append(&later);
        let state = fold(log.events());
        assert_eq!(state.active_members().collect::<Vec<_>>(), [a], "the founder is alone after the leave");
        assert_eq!(state.seat_of(&b), Some(seat), "the former member keeps its historical seat");
        assert!(live.serve(&log, &state, &a, &b), "the final exchange can deliver the leave");
        assert_eq!(live.poll_send(&log), Some(events_message(root.town, &[&before])), "an in-flight difference delivers the earlier edit");
        assert_eq!(live.poll_send(&log), Some(Message::HeadsDone { town: root.town, heads: &[before.id] }.encode()),
            "completion covers exactly the difference, before the live removal");
        assert_eq!(live.poll_send(&log), Some(events_message(root.town, &[log.get(&removal).unwrap()])), "live delivery includes the removal");
        assert!(live.poll_send(&log).is_none(), "live delivery withholds the later edit");

        let mut catch_up = SyncEngine::new();
        catch_up.connect(&log);
        assert!(catch_up.serve(&log, &state, &b, &a), "a leaver can deliver its own removal on a later link");
        let opening = catch_up.poll_send(&log).unwrap();
        let Some(Message::Heads { heads, .. }) = Message::decode(&opening).unwrap() else { panic!("expected opening Heads") };
        assert_eq!(heads, &[removal, before.id, accept, seat, root.id], "reconnect samples only the allowed prefix, nearest first");
        catch_up.on_message(&Message::Heads { town: root.town, heads: &[accept, later.id] }.encode(), &mut log).unwrap();
        assert_eq!(catch_up.poll_send(&log), Some(events_message(root.town, &[&before, log.get(&removal).unwrap()])),
            "a post-removal peer head confers no knowledge of the prefix");
        assert_eq!(catch_up.poll_send(&log), Some(Message::HeadsDone { town: root.town, heads: &[removal] }.encode()));
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
        let parent = chat(&key, root.town, vec![root.id], "parent");
        let child = chat(&key, root.town, vec![parent.id], "child");
        let received = engine.on_message(&events_message(root.town, &[&child]), &mut log).unwrap();
        assert_eq!(received.missing_parents, 1, "one record lacks a parent");
        assert!(log.events().len() == 1 && received.appended.is_empty(), "an orphan is never stored");
        assert_eq!(engine.poll_send(&log), Some(Message::Heads { town: root.town, heads: &[root.id] }.encode()), "the gap requests a fresh exchange");
        let received = engine.on_message(&events_message(root.town, &[&parent, &parent, &child]), &mut log).unwrap();
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
        let event = chat(&key, root.town, vec![root.id], "valid prefix");
        let owner = DeviceKey::generate().unwrap();
        let other = genesis(&owner);
        let mut elsewhere = dir.log("elsewhere", &other);
        let foreign = chat(&owner, other.town, vec![other.id], "other town");
        elsewhere.append(foreign.clone()).unwrap();
        let mut sender = SyncEngine::new();
        sender.on_message(&Message::Heads { town: other.town, heads: &[] }.encode(), &mut elsewhere).unwrap();
        let message = sender.poll_send(&elsewhere).unwrap();
        assert!(matches!(engine.on_message(&message, &mut log), Err(SyncError::Malformed(_))),
            "a complete sync from another town is refused");
        assert_eq!(log.events(), std::slice::from_ref(&root), "no foreign record was stored");
        let foreign_batch = events_message(root.town, &[&event, &foreign]);
        let outsider = chat(&owner, root.town, vec![event.id], "no own parent");
        let outsider_batch = events_message(root.town, &[&event, &outsider]);
        assert!(matches!(engine.on_message(&outsider_batch, &mut log), Err(SyncError::Event(_))),
            "a chat without an own parent has the same refusal as an invalid signature");
        let mut bad_sig = events_message(root.town, &[&event, &event]);
        *bad_sig.last_mut().unwrap() ^= 1;
        let mut bad_len = events_message(root.town, &[&event]);
        bad_len.extend_from_slice(&u32::MAX.to_le_bytes());
        let truncated = &bad_sig[..bad_sig.len() - 1];
        let mut unknown = Message::Heads { town: root.town, heads: &[] }.encode();
        unknown[1] = 0xff;
        for bytes in [&[VERSION + 1, HEADS][..], &unknown] {
            let got = engine.on_message(bytes, &mut log).expect("an unknown version or kind is ignored");
            assert!(got.appended.is_empty() && log.events().len() == 1, "an ignored message stores no records");
            assert!(engine.poll_send(&log).is_none(), "an ignored message schedules no reply");
        }
        let mut partial = Message::Heads { town: root.town, heads: &[] }.encode();
        partial.push(0);
        for (property, bytes) in [
            ("short envelope", &[VERSION][..]), ("partial head", partial.as_slice()),
            ("foreign town", &foreign_batch), ("missing own parent", &outsider_batch), ("short record", truncated), ("length beyond message", &bad_len), ("invalid signature", &bad_sig),
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
        let full = chat(&key, root.town, vec![root.id], "erase this");
        let erased = Event::from_parts(root.town, full.header_bytes.clone(), vec![], full.sig.clone()).unwrap();
        log.append(erased).unwrap();
        peer.append(full.clone()).unwrap();
        let mut engine = SyncEngine::new();
        let mut bytes = Message::Events { town: log.town_id(), records: &[] }.encode();
        bytes.extend_from_slice(&std::fs::read(dir.0.join("full")).unwrap());
        let received = engine.on_message(&bytes, &mut log).unwrap();
        assert!(received.appended.is_empty(), "the full copy is a duplicate");
        drop(log);
        let reopened = Log::open(&dir.0.join("redacted")).unwrap().0;
        assert!(reopened.get(&full.id).unwrap().redacted(), "the durable local copy stays redacted");
        assert!(engine.poll_send(&reopened).is_none(), "a duplicate alone needs no confirmation");
    }

    #[test]
    fn stored_duplicates_keep_membership_evidence_and_batch_order() {
        let dir = Temp::new("duplicate-membership");
        let ana = DeviceKey::generate().unwrap();
        let ben = DeviceKey::generate().unwrap();
        let root = genesis(&ana);
        let mut log = dir.log("town", &root);
        let invite = crate::log::tests::admit(&mut log, &ana, &ben);
        let invite = log.get(&invite).unwrap().clone();
        let accept = log.events().last().unwrap().clone();
        let mut engine = SyncEngine::new();
        let before = std::fs::read(dir.0.join("town")).unwrap();
        let received = engine.on_message(&events_message(root.town, &[&accept]), &mut log).unwrap();
        assert!(received.appended.is_empty());
        assert_eq!(received.delivered_membership, [accept.id], "even a stored Accept is evidence");

        let first = chat(&ben, root.town, vec![accept.id], "first");
        let next = chat(&ben, root.town, vec![first.id], "next");
        let receive_bad = |bad: &Event, log: &mut Log, engine: &mut SyncEngine| {
            engine.on_message(&events_message(root.town, &[&invite, &first, bad]), log).unwrap_err()
        };
        let mut bad = accept.clone();
        bad.sig[0] ^= 1;
        assert!(matches!(receive_bad(&bad, &mut log, &mut engine), SyncError::Event(FormatError::BadSignature)));
        bad = accept.clone();
        bad.body[0] ^= 1;
        assert!(matches!(receive_bad(&bad, &mut log, &mut engine), SyncError::Event(FormatError::BodyMismatch)));
        assert!(log.get(&first.id).is_none(), "an invalid duplicate cannot commit an earlier new record");
        assert_eq!(std::fs::read(dir.0.join("town")).unwrap(), before);

        let batch = events_message(root.town, &[&accept, &first, &invite, &first, &next, &accept]);
        let received = engine.on_message(&batch, &mut log).unwrap();
        assert_eq!(received.delivered_membership, [accept.id, invite.id, accept.id]);
        assert_eq!(received.appended, [first.id, next.id]);
        assert_eq!(received.missing_parents, 0, "the child sees its staged parent");
        assert_eq!(&log.events()[log.events().len() - 2..], &[first, next]);
    }

    #[test]
    fn difference_splits_at_record_boundaries_and_skips_known_ancestors() {
        let dir = Temp::new("split");
        let key = DeviceKey::generate().unwrap();
        let root = genesis(&key);
        let (mut log, mut peer) = (dir.log("sender", &root), dir.log("receiver", &root));
        for text in ["common root", "common head"] {
            let event = chat(&key, root.town, log.heads(), text);
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
            let event = chat(&key, root.town, log.heads(), &text);
            expected.push(event.id);
            log.append(event).unwrap();
        }
        let mut send = SyncEngine::new();
        send.on_message(&heads, &mut log).unwrap();
        let mut sizes = Vec::new();
        let mut appended = Vec::new();
        while let Some(bytes) = send.poll_send(&log) {
            let received = recv.on_message(&bytes, &mut peer).unwrap();
            assert_eq!(received.missing_parents, 0, "each split begins after all its parents");
            appended.extend(received.appended);
            if received.caught_up {
                assert_eq!(appended, expected, "completion follows the records");
            } else { sizes.push(bytes.len()); }
            assert_eq!(send.peer_heads_answered(), received.caught_up);
        }
        assert!(sizes.len() > 1 && sizes.iter().all(|n| *n <= BATCH_BYTES), "ordinary records respect the split target");
        assert_eq!(appended, expected, "all missing records arrive in log order");
        let disk = std::fs::read(dir.0.join("sender")).unwrap();
        let sent_record_bytes: usize = sizes.iter().map(|n| n - HEADER_BYTES).sum();
        assert_eq!(sent_record_bytes, disk.len() - common_bytes, "the common head and its ancestor were omitted");
        assert_eq!(std::fs::read(dir.0.join("receiver")).unwrap(), disk, "relayed records are byte exact");
        assert!(recv.poll_send(&peer).is_none(), "successful catch-up batches need no reply");

        let large = chat(&key, root.town, log.heads(), &"y".repeat(BATCH_BYTES));
        log.append(large.clone()).unwrap();
        recv.connect(&peer);
        send.on_message(&recv.poll_send(&peer).unwrap(), &mut log).unwrap();
        let bytes = send.poll_send(&log).unwrap();
        assert_eq!(recv.on_message(&bytes, &mut peer).unwrap().appended, [large.id], "one oversized record still arrives");
        let done = send.poll_send(&log).unwrap();
        assert!(recv.on_message(&done, &mut peer).unwrap().caught_up);
        assert!(send.poll_send(&log).is_none(), "the oversized record is sent once");
    }

    #[test]
    fn fork_final_exchange_gates_start_forks_strangers_and_both_siblings() {
        use crate::event::Member;
        use crate::fold::ForkPoint;

        let dir = Temp::new("fork-final");
        let [ana_key, ben_key, mallory_key, kit_key] = [(); 4].map(|_| DeviceKey::generate().unwrap());
        let (ana, ben, mallory) = (ana_key.public(), ben_key.public(), mallory_key.public());
        let root = genesis(&ana_key);
        let town = root.town;
        let mut log = dir.log("town", &root);
        let member = |key: &DeviceKey, op: Member, parents: Vec<Id>, salt: u8| {
            let content = Content::Member(op);
            let bytes = content.encode();
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical: true,
                actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents, content: &bytes };
            Event::create(key, town, draft, [salt; 16]).unwrap()
        };
        let invite = member(&ana_key, Member::Invite { subject: ben, role: 0 }, vec![root.id], 1);
        // Two copies of Ben's key each sign a first Accept; distinct salts make them siblings.
        let accept_1 = member(&ben_key, Member::Accept { entry: invite.id }, vec![invite.id], 2);
        let accept_2 = member(&ben_key, Member::Accept { entry: invite.id }, vec![invite.id], 3);
        log.append_all(vec![invite.clone(), accept_1.clone(), accept_2.clone()]).unwrap();
        let after = chat(&ana_key, town, log.parents_for(&ana), "after the fork");
        log.append(after.clone()).unwrap();
        let state = fold(log.events());
        assert_eq!(state.forked(&ben), Some(ForkPoint::Start), "start fork: Ben's two first Accepts fork it at Start");
        let end = log.position(&accept_2.id).map(|i| i + 1);
        assert_eq!(send_end(&log, &state, &ana, &ben), end, "start fork: the final prefix ends at the second Accept");

        let mut engine = SyncEngine::new();
        engine.connect(&log);
        assert!(engine.serve(&log, &state, &ana, &ben), "start fork: an invited start-forked peer gets a final exchange");
        assert!(engine.take_final_heads(&log).is_some(), "start fork: the final heads are queued once");
        assert_eq!(engine.shown_final(), [accept_1.id, accept_2.id], "start fork: both siblings are the shown evidence");
        assert!(engine.take_final_heads(&log).is_none(), "start fork: unchanged evidence queues no second frame");

        let mut answered = SyncEngine::new();
        answered.connect(&log);
        assert!(answered.serve(&log, &state, &ana, &ben), "both siblings: the final exchange is open");
        answered.on_message(&Message::Heads { town, heads: &[accept_2.id] }.encode(), &mut log).unwrap();
        let replies: Vec<_> = std::iter::from_fn(|| answered.poll_send(&log)).collect();
        let events: Vec<_> = replies.iter().filter(|b| matches!(Message::decode(b), Ok(Some(Message::Events { .. })))).collect();
        assert_eq!(events, [&events_message(town, &[&accept_1])],
            "both siblings: the difference sends only the missing sibling, never the post-fork chat");
        assert!(answered.finish_prefix(&log), "both siblings: a peer-heads link retires with its evidence");
        assert_eq!(answered.poll_send(&log), Some(events_message(town, &[&accept_1])), "both siblings: the first sibling is queued");
        assert_eq!(answered.poll_send(&log), Some(events_message(town, &[&accept_2])), "both siblings: the second sibling is queued");
        assert!(answered.poll_send(&log).is_none(), "both siblings: nothing after the fork is queued");

        let bogus_1 = member(&mallory_key, Member::Accept { entry: [0; 32] }, vec![root.id], 4);
        let bogus_2 = member(&mallory_key, Member::Accept { entry: [0; 32] }, vec![root.id], 5);
        log.append_all(vec![bogus_1, bogus_2]).unwrap();
        let silent = |log: &Log, state: &State, property: &str| {
            assert_eq!(send_end(log, state, &ana, &mallory), Some(0), "{property}: Mallory has no sendable prefix");
            let mut engine = SyncEngine::new();
            engine.connect(log);
            assert!(!engine.serve(log, state, &ana, &mallory), "{property}: Mallory is not served");
            assert!(engine.take_final_heads(log).is_none(), "{property}: no final heads go to Mallory");
            assert!(engine.shown_final().is_empty(), "{property}: no evidence is shown to Mallory");
            assert_eq!(engine.poll_send(log), Some(Message::Heads { town, heads: &[] }.encode()),
                "{property}: Mallory cannot see even the town's heads");
        };
        let state = fold(log.events());
        assert_eq!(state.forked(&mallory), Some(ForkPoint::Start), "stranger exclusion: Mallory's bogus Accepts fork at Start");
        silent(&log, &state, "stranger exclusion");

        let kit_entry = crate::log::tests::admit(&mut log, &ana_key, &kit_key);
        let leave = member(&ana_key, Member::Remove { seat: root.id, device: ana, last: kit_entry }, log.parents_for(&ana), 6);
        log.append(leave).unwrap();
        let state = fold(log.events());
        assert!(!state.active_member(&ana), "removed me: Ana's leave is counted");
        assert!(state.first_removal(&log, &ana, &mallory).is_some(), "removed me: Ana's own removal bounds every pair she is in");
        silent(&log, &state, "removed me");
        assert_eq!(send_end(&log, &state, &ana, &ben), end, "start fork: a later removal never extends the fork's prefix");
    }
}
