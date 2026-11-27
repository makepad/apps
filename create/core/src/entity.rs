//! Entity kinds and field registers. Numeric ids are permanent, and Splash names are open.

use crate::event::{Actor, Id, Key, Value};
use crate::fold::State;
use crate::key::PublicKey;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entity {
    pub id: Id,
    pub kind: Key,
    pub project: Option<Id>,
    pub author: PublicKey,
    pub actor: Actor,
    pub created: u64,
    pub last_activity: u64,
    pub fields: BTreeMap<Key, FieldState>,
    pub(crate) order_written: u64,
    pub(crate) z_written: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldState {
    pub value: Value,
    pub writer: Id,
}

impl Entity {
    /// Sort siblings by this pair. Concurrent insertions can write identical key bytes.
    pub fn order_key(&self) -> Option<(&[u8], u64)> {
        match self.value(field::ORDER) {
            Some(Value::Key(key)) => Some((key, self.order_written)),
            _ => None,
        }
    }

    /// Stack overlapping items by this pair, the topmost last. Like the order key, a later colour
    /// or text edit never changes it.
    pub fn z_key(&self) -> Option<(&[u8], u64)> {
        match self.value(field::Z) {
            Some(Value::Key(key)) => Some((key, self.z_written)),
            _ => None,
        }
    }

    pub(crate) fn write(&mut self, field: Key, value: Value, writer: Id, position: u64) -> Option<FieldState> {
        self.last_activity = position;
        if field == field::ORDER {
            self.order_written = position;
        }
        if field == field::Z {
            self.z_written = position;
        }
        // During replay even a clear needs a writer, so a later UNDO can test ownership.
        self.fields.insert(field, FieldState { value, writer })
    }

    /// Name keys follow all numeric keys and sort lexicographically by UTF-8 bytes.
    pub fn open_fields(&self) -> impl Iterator<Item = (&str, &FieldState)> {
        self.fields.range(Key::Name(String::new())..).filter_map(|(key, state)| {
            if let Key::Name(name) = key { Some((name.as_str(), state)) } else { None }
        })
    }

    pub fn value(&self, field: Key) -> Option<&Value> {
        self.fields.get(&field).map(|f| &f.value)
    }

    pub fn text(&self, field: Key) -> &str {
        match self.value(field) {
            Some(Value::Text(text)) => text,
            _ => "",
        }
    }

    pub fn flag(&self, field: Key) -> bool {
        self.value(field) == Some(&Value::Bool(true))
    }
}

fn links(state: &State, field: Key, id: Id) -> Vec<&Entity> {
    state.entities.values().filter(|e| {
        e.kind == kind::LINK && !e.flag(field::DELETED) && e.value(field.clone()) == Some(&Value::Id(id))
    }).collect()
}

pub fn backlinks(state: &State, id: Id) -> Vec<&Entity> {
    links(state, field::TO, id)
}

pub fn links_from(state: &State, id: Id) -> Vec<&Entity> {
    links(state, field::FROM, id)
}

pub fn project_entities(state: &State, project: Option<Id>, kind: Option<u16>) -> Vec<&Entity> {
    let mut entities: Vec<_> = state.visible_entities(move |e| {
        e.project == project && kind.is_none_or(|k| e.kind == k)
    }).collect();
    entities.sort_unstable_by_key(|e| (std::cmp::Reverse(e.last_activity), e.id));
    entities
}

pub fn derived_task(doc: Id, at: crate::doc::CharId) -> Id {
    makepad_network::digest::sha256_hash(&[b"makepad create derived task v1".as_slice(), &doc, &at.bytes()].concat())
}

pub mod kind {
    pub const PROJECT: u16 = 1;
    pub const TASK: u16 = 2;
    pub const BOARD: u16 = 3;
    pub const STICKY: u16 = 4;
    pub const TEXT_BLOCK: u16 = 5;
    pub const IMAGE: u16 = 6;
    pub const FILE: u16 = 7;
    pub const WEB_LINK: u16 = 8;
    pub const SHAPE: u16 = 9;
    pub const CONNECTOR: u16 = 10;
    pub const SECTION: u16 = 11;
    pub const STROKE: u16 = 12;
    pub const INLINE_APP: u16 = 13;
    pub const DOC: u16 = 14;
    pub const RECORDING: u16 = 15;
    /// On a CLIP, W and H are the media's pixel dimensions, or zero when unknown.
    pub const CLIP: u16 = 16;
    pub const SHOT: u16 = 17;
    pub const COMMIT_REF: u16 = 18;
    pub const LINK: u16 = 19;
    pub const COMMENT: u16 = 20;
    /// The town's own name: its `TITLE`, on an entity with no project. When two computers
    /// each make one, the lowest id is the name for everyone.
    pub const TOWN: u16 = 21;
    pub const DESIGN: u16 = 22;
    pub const APP: u16 = 24;
    pub const APP_EDIT: u16 = 25;
    pub const ISSUE: u16 = 26;
}

/// Coordinates and sizes (X, Y, W, H) are integers in 1/64 canvas units.
pub mod field {
    use super::Key;
    pub const TITLE: Key = Key::Number(1);
    pub const DELETED: Key = Key::Number(2);
    pub const ARCHIVED: Key = Key::Number(3);
    pub const PARENT: Key = Key::Number(4);
    /// Home board of a board item, used when its parent chain loops.
    pub const BOARD: Key = Key::Number(22);
    pub const ORDER: Key = Key::Number(5);
    pub const X: Key = Key::Number(6);
    pub const Y: Key = Key::Number(7);
    pub const W: Key = Key::Number(8);
    pub const H: Key = Key::Number(9);
    pub const Z: Key = Key::Number(10);
    pub const LOCKED: Key = Key::Number(11);
    pub const COLOUR: Key = Key::Number(12);
    pub const TEXT: Key = Key::Number(13);
    pub const BLOB: Key = Key::Number(14);
    pub const URL: Key = Key::Number(15);
    pub const FROM: Key = Key::Number(16);
    pub const TO: Key = Key::Number(17);
    pub const ANCHOR: Key = Key::Number(18);
    pub const STATUS: Key = Key::Number(19);
    pub const LABEL: Key = Key::Number(20);
    pub const SHAPE: Key = Key::Number(21);
    /// A design's Splash source. Its preset is not stored: it is read from the folded size.
    pub const SRC: Key = Key::Number(23);
    /// Blob for a clip's poster image.
    pub const POSTER: Key = Key::Number(24);
    /// Clip duration in milliseconds.
    pub const DURATION: Key = Key::Number(25);
    /// Text hint naming the clip's container and codec.
    pub const MEDIA: Key = Key::Number(26);
    /// Timeline session id of a clip or shared app.
    pub const SESSION: Key = Key::Number(27);

    pub fn geometry(key: &Key) -> bool { [X, Y, W, H, Z].contains(key) }

    /// Task id that surviving references follow after a merge.
    pub const MERGED_INTO: Key = Key::Number(28);
    pub const SHARER: Key = Key::Number(29);
    pub const EDIT_SEAT: Key = Key::Number(30);
    /// Read only, for AI edits saved before EDIT_AGENT. New edits never write this field.
    pub const EDIT_AI: Key = Key::Number(31);
    pub const WIDGET_PATH: Key = Key::Number(32);
    pub const PROPERTY: Key = Key::Number(33);
    pub const OLD_VALUE: Key = Key::Number(34);
    pub const NEW_VALUE: Key = Key::Number(35);
    pub const EDIT_SEQ: Key = Key::Number(36);

    /// A task proposed by a hub service. It is hidden from AI members until a person accepts it.
    pub const PROPOSED: Key = Key::Number(37);
    /// The agent that made a shared app edit on its owner's seat.
    pub const EDIT_AGENT: Key = Key::Number(38);
    /// Blob for an issue's video recording.
    pub const VIDEO: Key = Key::Number(39);
}

use crate::fold::cache::fields as cache_fields;
cache_fields!(Entity { id, kind, project, author, actor, created, last_activity, fields, order_written, z_written });
cache_fields!(FieldState { value, writer });
