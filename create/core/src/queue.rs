//! What waits for my AI: a durable local queue, Town's own state, never shared. Its items are
//! derived from the log in the order this machine got them, so nothing is lost when Town or the
//! agent restarts: an item is my own human message addressed to my AI (`@`). Later kinds (a task
//! assigned to my AI, a consent request) join the same list.
//!
//! The agent's `wait after=<last id>` returns everything pending after that id in one answer, and
//! naming the id is the acknowledgement: an agent that died before acting gets the item again.
//! Nothing expires by age.

use crate::event::{kind, Actor, Content, Id};
use crate::fold;
use crate::fold::cache::{fields, Cached};
use std::collections::{BTreeMap, BTreeSet};
use crate::home_writer::PrivateFiles;
use crate::key::PublicKey;
use crate::log::Log;
use std::io;
use std::path::{Path, PathBuf};

/// The items for `me`'s AI, in the order this machine got them.
pub fn items(log: &Log, me: &PublicKey) -> Vec<Id> {
    log.events()
        .iter()
        .filter(|event| {
            let h = &event.header;
            if h.kind != kind::CHAT {
                return false;
            }
            let Some(content) = event.content() else { return false };
            match Content::decode(h.kind, h.kind_version, content) {
                Some(Content::Chat(chat)) => {
                    let msg = fold::ChatMsg { id: event.id, author: h.author, actor: h.actor, via: h.via, wall_ms: h.wall_ms, chat, author_agent: None, to_agent: None };
                    fold::addresses_my_ai(&msg, me)
                }
                _ => false,
            }
        })
        .map(|event| event.id)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownId;

/// The items after `after` (all of them when `None`).
pub fn pending<'a>(items: &'a [Id], after: Option<&Id>) -> Result<&'a [Id], UnknownId> {
    match after {
        None => Ok(items),
        Some(after) => items.iter().position(|id| id == after).map(|at| &items[at + 1..]).ok_or(UnknownId),
    }
}

/// The last id the agent acknowledged, kept in the home.
pub struct Acks {
    files: PrivateFiles,
    path: PathBuf,
    last: Option<Id>,
}

impl Acks {
    #[cfg(test)]
    pub fn load(path: &Path) -> Acks {
        Self::load_in(path, PrivateFiles::default())
    }

    fn load_in(path: &Path, files: PrivateFiles) -> Acks {
        let last = files.read(path).ok().and_then(|bytes| crate::parse_id(std::str::from_utf8(&bytes).ok()?));
        Acks { files, path: path.to_path_buf(), last }
    }

    pub fn for_town(home: &crate::home::Home, log: &Log) -> Self {
        let old = home.acks_path();
        let path = old.with_file_name(format!("ai-acked-{}", crate::hex(&log.town_id())));
        let mut cursor = Self::load_in(&path, home.files());
        if !home.files().exists(&path).unwrap_or(false) {
            cursor.last = Self::load_in(&old, home.files()).last.filter(|id| log.get(id).is_some());
        }
        cursor
    }

    pub fn last(&self) -> Option<&Id> {
        self.last.as_ref()
    }

    pub fn store(&mut self, id: Id) -> io::Result<()> {
        if self.last == Some(id) {
            return Ok(());
        }
        self.files.replace(&self.path, crate::hex(&id).into_bytes())?;
        self.last = Some(id);
        Ok(())
    }
}

/// A bearer value carries its town so requests can resolve it independently of the window.
/// Only its check value is retained by Town, including while a call is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Credential {
    pub town: Id,
    pub check: Id,
}

impl Credential {
    pub fn issue(town: Id) -> io::Result<(Self, String)> {
        let mut secret = [0; 32];
        makepad_network::tls::os_random(&mut secret)?;
        Ok((Self::from_secret(town, &secret), format!("{}.{}", crate::hex(&town), crate::hex(&secret))))
    }

    fn from_secret(town: Id, secret: &Id) -> Self {
        Self { town, check: makepad_network::digest::sha256_hash(secret) }
    }

    pub fn parse(value: &str) -> Option<Self> {
        let (town, secret) = value.split_once('.')?;
        if town.len() != 64 || secret.len() != 64 { return None }
        Some(Self::from_secret(crate::parse_id(town)?, &crate::parse_id(secret)?))
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct JoinCode {
    pub existing: Option<Id>,
    /// Written before appending a new Agent. A retry after an interrupted spend
    /// can finish that same creation if it reached the log.
    pub creating: Option<Id>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub row: usize,
    pub part: u8,
    pub offset: usize,
}

impl Position {
    pub fn cursor(self, agent: &Id) -> Id {
        let mut bytes = agent.to_vec();
        self.save(&mut bytes);
        makepad_network::digest::sha256_hash(&bytes)
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Inbox {
    pub binding: Id,
    pub last_heard: u64,
    /// Log length when the last wait delivered. A later signed reply clears answering.
    pub delivered_at: Option<usize>,
    pub approved: BTreeSet<Id>,
    pub declined: BTreeSet<Id>,
    /// Once eligible a line keeps its place, even if the fold later gains an earlier event.
    pub ready: Vec<Id>,
    pub ack: Position,
    pub offered: Option<Position>,
}

impl Inbox {
    pub fn acknowledge(&mut self, agent: &Id, after: Id) -> Result<(), String> {
        if let Some(offered) = self.offered.filter(|p| p.cursor(agent) == after) {
            self.ack = offered;
            self.offered = None;
        } else if self.ack.cursor(agent) != after {
            return Err("That cursor was not delivered to this agent.".into());
        }
        Ok(())
    }

    /// Takes the owner's bare `@` lines that no AI of theirs has answered: those after the owner's
    /// last AI line, oldest first. An AI that joins or comes back reads them first, so a line typed
    /// while no AI listened is not lost.
    pub fn take_unanswered(&mut self, state: &fold::State, owner: &PublicKey) {
        let mut known: BTreeSet<_> = self.ready.iter().copied().collect();
        let Some(seat) = state.seat_of(owner) else { return };
        for m in &state.chat[unanswered_from(state, owner)..] {
            if state.seat_of(&m.author) == Some(seat) && m.actor == Actor::Human && m.chat.to_ai && m.to_agent.is_none() && !state.void_events.contains_key(&m.id) && known.insert(m.id) {
                self.ready.push(m.id);
            }
        }
    }

    pub fn collect(&mut self, state: &fold::State, agent: &Id, owner: &PublicKey) {
        let seat = state.seat_of(owner);
        let mut known: BTreeSet<_> = self.ready.iter().copied().collect();
        for id in state.mentions(agent) {
            if let Some(message) = state.message(id) {
                if (seat.is_some() && state.seat_of(&message.author) == seat || self.approved.contains(id)) && known.insert(*id) {
                    self.ready.push(*id);
                }
            }
        }
    }
}

/// Where the owner's unanswered chat starts: just after the owner's last AI line.
pub fn unanswered_from(state: &fold::State, owner: &PublicKey) -> usize {
    let Some(seat) = state.seat_of(owner) else { return state.chat.len() };
    state.chat.iter().rposition(|m| state.seat_of(&m.author) == Some(seat) && m.actor == Actor::Ai).map_or(0, |at| at + 1)
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct AgentBook {
    pub codes: BTreeMap<Id, JoinCode>,
    pub inboxes: BTreeMap<Id, Inbox>,
}

fields!(JoinCode { existing, creating });
fields!(Position { row, part, offset });
fields!(Inbox { binding, last_heard, delivered_at, approved, declined, ready, ack, offered });
fields!(AgentBook { codes, inboxes });

impl AgentBook {
    const VERSION: &'static [u8] = b"work local agents v1\0";

    #[cfg(test)]
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_in(path, &PrivateFiles::default())
    }

    pub fn open_in(path: &Path, files: &PrivateFiles) -> io::Result<Self> {
        let bytes = match files.read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "unreadable local agent record");
        let mut input = bytes.strip_prefix(Self::VERSION).ok_or_else(invalid)?;
        let book = Self::load(&mut input).ok_or_else(invalid)?;
        if !input.is_empty() { return Err(invalid()) }
        Ok(book)
    }

    #[cfg(test)]
    pub fn store(&self, path: &Path) -> io::Result<()> {
        self.store_in(path, &PrivateFiles::default())
    }

    pub fn store_in(&self, path: &Path, files: &PrivateFiles) -> io::Result<()> {
        let mut bytes = Self::VERSION.to_vec();
        self.save(&mut bytes);
        files.replace(path, bytes)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Turn {
    pub line: Id,
    /// Arrival boundary of the offered page, retained for a wait retry.
    pub through: usize,
    pub foreign: bool,
    /// Arrival watermark and exclusive lower bound for each returned timeline continuation.
    pub pages: BTreeMap<String, (usize, Option<Id>)>,
}

fields!(Turn { line, through, foreign, pages });

/// Private turn state for one town. The v1 inbox's offered position identifies its page.
pub struct AgentTurns {
    files: PrivateFiles,
    path: PathBuf,
    offered: BTreeMap<Id, (Position, Turn)>,
}

impl AgentTurns {
    const VERSION: &'static [u8] = b"work local agent turns v1\0";

    #[cfg(test)]
    pub fn open(path: PathBuf) -> Self {
        Self::open_in(path, PrivateFiles::default())
    }

    pub fn open_in(path: PathBuf, files: PrivateFiles) -> Self {
        let offered = files.read(&path).ok().and_then(|bytes| {
            let mut input = bytes.strip_prefix(Self::VERSION)?;
            let turns = BTreeMap::load(&mut input)?;
            input.is_empty().then_some(turns)
        }).unwrap_or_default();
        Self { files, path, offered }
    }

    pub fn get(&self, agent: &Id, offered: Option<Position>) -> Option<&Turn> {
        let (saved, turn) = self.offered.get(agent)?;
        (Some(*saved) == offered).then_some(turn)
    }

    pub fn has_foreign(&self, book: &AgentBook) -> bool {
        self.offered.iter().any(|(agent, (position, turn))| turn.foreign
            && book.inboxes.get(agent).and_then(|inbox| inbox.offered) == Some(*position))
    }

    pub fn set(&mut self, agent: Id, next: Option<(Position, Turn)>) -> io::Result<()> {
        if self.offered.get(&agent) == next.as_ref() { return Ok(()) }
        let mut offered = self.offered.clone();
        match next {
            Some(page) => { offered.insert(agent, page); }
            None => { offered.remove(&agent); }
        }
        let mut bytes = Self::VERSION.to_vec();
        offered.save(&mut bytes);
        self.files.replace(&self.path, bytes)?;
        self.offered = offered;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, Chat, Draft, Event, Via};
    use crate::key::DeviceKey;

    #[test]
    fn the_queue_holds_my_at_messages_until_acknowledged() {
        let dir = std::env::temp_dir().join(format!("makepad-create-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (me, peer) = (DeviceKey::generate().unwrap(), DeviceKey::generate().unwrap());
        let (_, genesis) = Event::found(&me, 1, 0, 0, [0; 16]).unwrap();
        let mut log = Log::found(&dir.join("log"), genesis).unwrap();
        crate::log::tests::admit(&mut log, &me, &peer);
        let post = |log: &mut Log, key: &DeviceKey, actor: Actor, text: &str, to_ai: bool| {
            let content = Content::Chat(Chat { text: text.into(), to_ai, ..Chat::default() }).encode();
            let draft = Draft { kind: 2, kind_version: 1, critical: false, actor, via: Via::Ui, wall_ms: 0, parents: log.parents_for(&key.public()), content: &content };
            let event = Event::create(key, log.town_id(), draft, [text.len() as u8; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            id
        };
        let first = post(&mut log, &me, Actor::Human, "@ai one", true);
        post(&mut log, &me, Actor::Human, "no at", false);
        post(&mut log, &peer, Actor::Human, "@ai from the peer", true);
        post(&mut log, &me, Actor::Ai, "my AI's own words", true);
        let second = post(&mut log, &me, Actor::Human, "@ai two", true);
        let all = items(&log, &me.public());
        assert_eq!(all, vec![first, second], "only my human @ messages wait for my AI");
        assert_eq!(pending(&all, None), Ok(&all[..]), "nothing acknowledged: everything waits");
        assert_eq!(pending(&all, Some(&first)), Ok(&all[1..]), "after the first");
        assert_eq!(pending(&all, Some(&second)), Ok(&all[2..]), "after the last: nothing");
        assert_eq!(pending(&all, Some(&[0; 32])), Err(UnknownId), "an id that isn't an item");

        let mut acks = Acks::load(&dir.join("ai-acked"));
        assert_eq!(acks.last(), None);
        acks.store(first).unwrap();
        assert_eq!(Acks::load(&dir.join("ai-acked")).last(), Some(&first), "the acknowledgement survives a restart");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
