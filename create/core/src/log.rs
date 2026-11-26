//! The log on disk: events appended as raw bytes, never rewritten, in the order this machine got
//! them. Each record is `header_len u32 | header | body_len u32 | body | sig_len u16 | sig`
//! (little-endian). The first record founds the room. Later records name their author’s previous event;
//! a new author starts with an accept. What the app shows is folded from the verified room
//! ([`crate::fold`]) and never stored as truth.

use crate::event::{kind, Content, Event, FormatError, Id, Member, RoomId};
use crate::key::PublicKey;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug)]
pub enum OpenError {
    NotARoom,
    Io(io::Error),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::NotARoom => f.write_str("the log has no verified room genesis"), Self::Io(e) => e.fmt(f) }
    }
}

impl std::error::Error for OpenError {}
impl From<io::Error> for OpenError {
    fn from(e: io::Error) -> Self { Self::Io(e) }
}

pub struct Log {
    room: RoomId,
    latest: HashMap<PublicKey, Id>,
    file: File,
    // If a write and its rollback both fail, repair the tail before any subsequent append.
    rollback_at: Option<u64>,
    events: Vec<Event>,
    index: HashMap<Id, usize>,
    /// Ids some event names as a parent: the rest are the heads.
    cited: HashSet<Id>,
}

impl Log {
    fn empty(file: File, room: RoomId) -> Self {
        Self { file, room, latest: HashMap::new(), rollback_at: None, events: vec![], index: HashMap::new(), cited: HashSet::new() }
    }

    pub fn room_id(&self) -> RoomId { self.room }
    pub fn genesis(&self) -> &Event { &self.events[0] }

    fn read_genesis(header: &[u8], body: &[u8], sig: &[u8]) -> Result<Event, OpenError> {
        let room = Event::genesis_room(header);
        let event = Event::from_parts(room, header.to_vec(), body.to_vec(), sig.to_vec()).map_err(|_| OpenError::NotARoom)?;
        if event.header.kind != kind::ROOM || !event.content().and_then(|c| Content::decode(kind::ROOM, event.header.kind_version, c))
            .is_some_and(|c| matches!(c, Content::Room { .. })) {
            return Err(OpenError::NotARoom)
        }
        Ok(event)
    }

    /// Inspect a room without opening it for repair or reading the rest of its history.
    pub fn inspect_genesis(path: &Path) -> Result<Event, OpenError> {
        let mut file = File::open(path)?;
        let mut part = |width: usize| -> Result<Vec<u8>, OpenError> {
            let mut len = [0; 4];
            file.read_exact(&mut len[..width]).map_err(|e| match e.kind() {
                io::ErrorKind::UnexpectedEof => OpenError::NotARoom,
                _ => OpenError::Io(e),
            })?;
            let len = u32::from_le_bytes(len) as u64;
            let mut bytes = Vec::new();
            (&mut file).take(len).read_to_end(&mut bytes)?;
            if bytes.len() as u64 != len { return Err(OpenError::NotARoom) }
            Ok(bytes)
        };
        Self::read_genesis(&part(4)?, &part(4)?, &part(2)?)
    }

    /// Creates a new log starting with its verified genesis. An existing path is preserved.
    pub fn found(path: &Path, genesis: Event) -> io::Result<Self> {
        let genesis = Self::read_genesis(&genesis.header_bytes, &genesis.body, &genesis.sig)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
        let mut log = Self::empty(file, genesis.room);
        let mut bytes = vec![];
        encode_record(&genesis, &mut bytes);
        if let Err(e) = log.file.write_all(&bytes).and_then(|_| log.file.sync_data()) {
            drop(log);
            std::fs::remove_file(path)?;
            return Err(e)
        }
        log.push(genesis);
        Ok(log)
    }

    /// Verifies the first record before repairing a torn tail. A legacy file is never modified.
    pub fn open(path: &Path) -> Result<(Log, Vec<String>), OpenError> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let ((h, b, s), mut at) = record(&bytes).ok_or(OpenError::NotARoom)?;
        let genesis = Self::read_genesis(h, b, s)?;
        let mut log = Self::empty(file, genesis.room);
        log.push(genesis);
        let mut problems = Vec::new();
        while at < bytes.len() {
            let Some((parts, next)) = record(&bytes[at..]) else {
                problems.push(format!("the log ended inside a record at byte {at}: removed the partial record"));
                log.file.set_len(at as u64)?;
                break;
            };
            let (header, body, sig) = parts;
            match Event::from_parts(log.room, header.to_vec(), body.to_vec(), sig.to_vec()).and_then(|event| {
                if !log.index.contains_key(&event.id) {
                    log.check_chain(&event, log.latest.get(&event.header.author))?;
                    log.push(event);
                }
                Ok(())
            }) {
                Ok(()) => {},
                Err(e) => problems.push(format!("skipped the record at byte {at}: {e}")),
            }
            at += next;
        }
        Ok((log, problems))
    }

    fn push(&mut self, event: Event) {
        if self.index.contains_key(&event.id) {
            return;
        }
        self.latest.insert(event.header.author, event.id);
        self.cited.extend(event.header.parents.iter().copied());
        self.index.insert(event.id, self.events.len());
        self.events.push(event);
    }

    fn check_chain(&self, event: &Event, previous: Option<&Id>) -> Result<(), FormatError> {
        if event.room != self.room || event.header.kind == kind::ROOM {
            return Err(FormatError::Malformed("event belongs to another room or repeats genesis"));
        }
        if let Some(id) = previous {
            if event.header.parents.contains(id) { return Ok(()) }
            return Err(FormatError::Malformed("event omits author's previous event"));
        }
        if event.header.kind == kind::MEMBER && matches!(
            event.content().and_then(|b| Content::decode(kind::MEMBER, event.header.kind_version, b)),
            Some(Content::Member(Member::Accept { .. }))) {
            Ok(())
        } else {
            Err(FormatError::Malformed("first event of a device must be its accept"))
        }
    }

    /// Validate a whole batch before its first disk or in-memory write.
    pub(crate) fn check_batch(&self, events: &[Event]) -> Result<(), FormatError> {
        let mut latest = HashMap::new();
        for event in events {
            let author = event.header.author;
            self.check_chain(event, latest.get(&author).or_else(|| self.latest.get(&author)))?;
            latest.insert(author, event.id);
        }
        Ok(())
    }

    /// Appends `event` to the file (flushed to disk) and to the log. An event already there is
    /// left alone.
    pub fn append(&mut self, event: Event) -> io::Result<()> {
        self.append_all(vec![event])
    }

    /// Writes new records in input order, syncs once, then makes them visible in memory.
    /// The first copy of an id wins, including among records in this batch.
    pub fn append_all(&mut self, mut events: Vec<Event>) -> io::Result<()> {
        let mut seen = HashSet::new();
        let single = events.len() == 1;
        events.retain(|e| !self.index.contains_key(&e.id) && (single || seen.insert(e.id)));
        if events.is_empty() {
            return Ok(());
        }
        self.check_batch(&events).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let mut bytes = Vec::new();
        for event in &events {
            encode_record(event, &mut bytes);
        }
        if let Some(at) = self.rollback_at {
            self.file.set_len(at)?;
            self.rollback_at = None;
        }
        let start = self.file.seek(SeekFrom::End(0))?;
        if let Err(error) = self.file.write_all(&bytes).and_then(|()| self.file.sync_data()) {
            if self.file.set_len(start).is_err() {
                self.rollback_at = Some(start);
            }
            return Err(error);
        }
        for event in events {
            self.push(event);
        }
        Ok(())
    }

    /// Every event, in the order this machine got them.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn get(&self, id: &Id) -> Option<&Event> {
        self.index.get(id).map(|&i| &self.events[i])
    }

    /// Zero-based arrival position, used to bound what a link may send.
    pub fn position(&self, id: &Id) -> Option<usize> {
        self.index.get(id).copied()
    }

    pub(crate) fn heads_before(&self, end: usize) -> Vec<Id> {
        if end >= self.events.len() { return self.heads() }
        let events = &self.events[..end];
        let cited: HashSet<_> = events.iter().flat_map(|e| &e.header.parents).collect();
        let mut heads: Vec<_> = events.iter().map(|e| e.id).filter(|id| !cited.contains(id)).collect();
        heads.sort();
        heads
    }

    /// The events no other event names as a parent.
    pub fn heads(&self) -> Vec<Id> {
        let mut heads: Vec<Id> = self.events.iter().map(|e| e.id).filter(|id| !self.cited.contains(id)).collect();
        heads.sort();
        heads
    }

    /// The parents of `author`'s next event: every head, and the author's own last event (an
    /// author's events form a chain, so a forked history shows).
    pub fn parents_for(&self, author: &PublicKey) -> Vec<Id> {
        let mut parents = self.heads();
        if let Some(last) = self.latest.get(author) {
            if !parents.contains(last) {
                parents.push(*last);
                parents.sort();
            }
        }
        parents
    }
}

// Our own events fit these prefixes: `Event::signed` refuses a header or body over u32::MAX
// bytes, and a signature has a fixed length far below u16::MAX.
pub(crate) fn encode_record(event: &Event, out: &mut Vec<u8>) {
    out.reserve(10 + event.header_bytes.len() + event.body.len() + event.sig.len());
    out.extend_from_slice(&(event.header_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&event.header_bytes);
    out.extend_from_slice(&(event.body.len() as u32).to_le_bytes());
    out.extend_from_slice(&event.body);
    out.extend_from_slice(&(event.sig.len() as u16).to_le_bytes());
    out.extend_from_slice(&event.sig);
}

/// One record at the start of `bytes`: its three parts and its length, or `None` when it is cut off.
pub(crate) fn record(bytes: &[u8]) -> Option<((&[u8], &[u8], &[u8]), usize)> {
    let mut at: usize = 0;
    let mut take = |n: usize| -> Option<&[u8]> {
        let end = at.checked_add(n)?;
        let part = bytes.get(at..end)?;
        at = end;
        Some(part)
    };
    let header_len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    let header = take(header_len)?;
    let body_len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
    let body = take(body_len)?;
    let sig_len = u16::from_le_bytes(take(2)?.try_into().ok()?) as usize;
    let sig = take(sig_len)?;
    Some(((header, body, sig), at))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::event::{Actor, Chat, Content, Draft, Via};
    use crate::key::DeviceKey;

    pub(crate) fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("makepad-create-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn admit(log: &mut Log, inviter: &DeviceKey, device: &DeviceKey) -> Id {
        let append = |log: &mut Log, key: &DeviceKey, op| {
            let bytes = Content::Member(op).encode();
            let draft = Draft { kind: kind::MEMBER, kind_version: 1, critical: true, actor: Actor::Human, via: Via::Ui,
                wall_ms: 0, parents: log.parents_for(&key.public()), content: &bytes };
            let event = Event::create(key, log.room_id(), draft, [0; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            id
        };
        let entry = append(log, inviter, Member::Invite { subject: device.public(), role: 0 });
        append(log, device, Member::Accept { entry });
        entry
    }

    fn say(key: &DeviceKey, log: &Log, text: &str) -> Event {
        let content = Content::Chat(Chat { text: text.into(), ..Chat::default() }).encode();
        let draft = Draft { kind: 2, kind_version: 1, critical: false, actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents: log.parents_for(&key.public()), content: &content };
        Event::create(key, log.room_id(), draft, [text.len() as u8; 16]).unwrap()
    }

    #[test]
    fn future_header_extensions_verify_fold_as_unknown_and_relay_verbatim() {
        use crate::event::{FormatError, Key};
        use crate::key::Domain;
        use makepad_network::digest::sha256_hash;

        let dir = temp_dir("future-header");
        let path = dir.join("received");
        let key = DeviceKey::generate().unwrap();
        let (room, genesis) = Event::found(&key, 2, 0, 0, [0; 16]).unwrap();
        let mut log = Log::found(&path, genesis.clone()).unwrap();
        let content = Content::Entity { kind: Key::Name("FutureCard".into()), project: None, derive: None, fields: vec![] };
        let mut ids = Vec::new();
        for (flags, tail) in [(0x80, false), (0, true), (0x80, true)] {
            let base = Event::create(&key, room, Draft { kind: content.kind(), kind_version: content.version(), critical: false,
                actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents: log.heads(), content: &content.encode() }, [1; 16]).unwrap();
            let mut wire = base.header_bytes.clone();
            wire[4] |= flags;
            if tail {
                // An empty future frame and a frame whose bytes need no interpretation.
                wire.extend_from_slice(&[0, 0, 0, 0, 3, 0, 0, 0, 0, 0xff, 7]);
            }
            let id = sha256_hash(&wire);
            let sig = key.sign(Domain::Event, &[room.as_slice(), &wire].concat()).unwrap();
            let event = Event::from_parts(room, wire.clone(), base.body.clone(), sig.clone()).unwrap();
            assert_eq!(event.id, id);
            assert_eq!(event.header.encode(), wire);
            assert_eq!(event.header.shape, base.header.shape, "shape ends before later extensions");
            assert!(event.content().is_none() && !event.redacted());
            assert_eq!(Event::from_parts(room, wire.clone(), vec![0; 16], sig), Err(FormatError::BodyMismatch));
            assert_eq!(Event::from_parts(room, wire, base.body.clone(), base.sig.clone()), Err(FormatError::BadSignature));
            ids.push(id);
            log.append(event).unwrap();
        }
        let child = say(&key, &log, "after all three parents");
        log.append(child.clone()).unwrap();
        drop(log);
        let (received, problems) = Log::open(&path).unwrap();
        assert!(problems.is_empty());
        let state = crate::fold::fold(received.events());
        assert_eq!(state.unknown, ids);
        assert!(state.entities.is_empty(), "even the recognizable ENTITY has no effect");
        assert_eq!(state.chat[0].id, child.id, "all opaque parents resolved");
        let copy = dir.join("relayed");
        let mut relayed = Log::found(&copy, genesis).unwrap();
        for event in received.events() { relayed.append(event.clone()).unwrap(); }
        assert_eq!(std::fs::read(path).unwrap(), std::fs::read(copy).unwrap());
        drop((received, relayed));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_log_reopens_as_written_and_drops_a_torn_tail() {
        let dir = temp_dir("log");
        let path = dir.join("log");
        let (me, peer) = (DeviceKey::generate().unwrap(), DeviceKey::generate().unwrap());
        std::fs::write(&path, []).unwrap();
        assert!(matches!(Log::open(&path), Err(OpenError::NotARoom)), "an empty file has no genesis");
        std::fs::remove_file(&path).unwrap();
        let (_, genesis) = Event::found(&me, 1, 0, 0, [0; 16]).unwrap();
        let mut log = Log::found(&path, genesis.clone()).unwrap();
        assert_eq!(log.genesis(), &genesis, "the genesis is the first record");
        let outsider = say(&peer, &log, "outside");
        assert!(log.append(outsider).is_err(), "a first event must be an accept");
        admit(&mut log, &me, &peer);
        let first_ids: Vec<_> = log.events().iter().map(|e| e.id).collect();
        let a = say(&me, &log, "a");
        log.append(a.clone()).unwrap();
        let first_len = std::fs::metadata(&path).unwrap().len();
        let b = say(&peer, &log, "b");
        log.append_all(vec![a.clone(), b.clone(), b.clone()]).unwrap();
        let second_len = (10 + b.header_bytes.len() + b.body.len() + b.sig.len()) as u64;
        assert_eq!(std::fs::metadata(&path).unwrap().len(), first_len + second_len,
            "a batch writes only the first new copy of each id to disk");
        assert_eq!(log.events().len(), first_ids.len() + 2, "an event is kept once");
        assert_eq!(log.heads(), vec![b.id], "the newest is the head");
        let fork = say(&me, &log, "other successor");
        let c = say(&me, &log, "c");
        assert_eq!(c.header.parents, { let mut p = vec![a.id, b.id]; p.sort(); p }, "the author's own last event is a parent beside the heads");
        log.append(c.clone()).unwrap();
        assert!(log.append(fork.clone()).is_err(), "a fork cannot replace the accepted successor");
        drop(log);

        let (log, problems) = Log::open(&path).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(log.events().iter().map(|e| e.id).collect::<Vec<_>>(), [first_ids.as_slice(), &[a.id, b.id, c.id]].concat(), "the same events, in the same order");
        assert_eq!(log.get(&b.id).map(|e| &e.body), Some(&b.body), "found by id");
        drop(log);

        let mut rejected = Vec::new();
        encode_record(&fork, &mut rejected);
        let len = std::fs::metadata(&path).unwrap().len() + rejected.len() as u64;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&rejected).unwrap();
        file.write_all(&[200, 0, 0, 0, 1, 2, 3]).unwrap();
        drop(file);
        let (log, problems) = Log::open(&path).unwrap();
        assert_eq!(problems.len(), 2, "the fork and torn tail are both reported: {problems:?}");
        assert!(log.get(&fork.id).is_none(), "a fork on disk is skipped");
        assert_eq!(log.events().len(), first_ids.len() + 3, "the valid records stay");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), len, "the torn tail is cut off");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_event_with_too_many_parents_is_refused_before_anything_is_signed_or_written() {
        let dir = temp_dir("parents");
        let path = dir.join("log");
        let me = DeviceKey::generate().unwrap();
        let (_, genesis) = Event::found(&me, 1, 0, 0, [0; 16]).unwrap();
        let log = Log::found(&path, genesis).unwrap();
        let len = std::fs::metadata(&path).unwrap().len();
        let content = Content::Chat(Chat { text: "x".into(), to_ai: false, quote: None, tasks: vec![] }).encode();
        let draft = |count: usize| Draft { kind: 2, kind_version: 1, critical: false, actor: Actor::Human, via: Via::Ui,
            wall_ms: 0, parents: (0..count).map(|n| { let mut id = [0; 32]; id[..8].copy_from_slice(&(n as u64).to_le_bytes()); id }).collect(),
            content: &content };
        let error = Event::create(&me, log.room_id(), draft(u16::MAX as usize + 1), [1; 16]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "65,536 parents don't fit the u16 count: {error}");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), len, "no bytes were written");
        assert_eq!(log.events().len(), 1, "nothing entered the log");
        let widest = Event::create(&me, log.room_id(), draft(u16::MAX as usize), [1; 16]).unwrap();
        assert_eq!(widest.header.parents.len(), u16::MAX as usize, "the widest event keeps its whole count");
        assert_eq!(Event::from_parts(widest.room, widest.header_bytes.clone(), widest.body.clone(), widest.sig.clone()).unwrap(), widest,
            "and reads back as signed");
        drop(log);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
