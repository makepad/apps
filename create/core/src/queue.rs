//! What waits for my AI: a durable local queue, Create's own state, never shared. Its items are
//! derived from the log in the order this machine got them, so nothing is lost when Create or the
//! agent restarts: an item is my own human message addressed to my AI (`@`). Later kinds (a task
//! assigned to my AI, a consent request) join the same list.
//!
//! The agent's `wait after=<last id>` returns everything pending after that id in one answer, and
//! naming the id is the acknowledgement: an agent that died before acting gets the item again.
//! Nothing expires by age.

use crate::event::{kind, Content, Id};
use crate::fold;
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
                    let msg = fold::ChatMsg { id: event.id, author: h.author, actor: h.actor, via: h.via, wall_ms: h.wall_ms, chat };
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
    path: PathBuf,
    last: Option<Id>,
}

impl Acks {
    pub fn load(path: &Path) -> Acks {
        let last = std::fs::read_to_string(path).ok().and_then(|text| crate::parse_id(&text));
        Acks { path: path.to_path_buf(), last }
    }

    pub fn for_room(home: &crate::home::Home, log: &Log) -> Self {
        let old = home.acks_path();
        let path = old.with_file_name(format!("ai-acked-{}", crate::hex(&log.room_id())));
        let mut cursor = Self::load(&path);
        if !path.exists() {
            cursor.last = Self::load(&old).last.filter(|id| log.get(id).is_some());
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
        crate::home::write_private(&self.path, crate::hex(&id).as_bytes())?;
        self.last = Some(id);
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
            let event = Event::create(key, log.room_id(), draft, [text.len() as u8; 16]).unwrap();
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
