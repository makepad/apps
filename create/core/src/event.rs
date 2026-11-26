//! The event format, v1. An event is three byte strings, kept exactly as written or received:
//!
//! - the **header**, signed and hashed: format, kind and its version, flags (critical, actor,
//!   via, public shape), the author's public key, the wall time (display only), the parents and the hash of the
//!   body. The event's id is SHA-256 of the header alone: ECDSA signatures aren't unique, and an id
//!   that covered one would let anyone mint "new" events from old ones by signing again.
//! - the **body**: 16 random bytes of salt, then the content. Redaction erases the body and keeps
//!   the header, so the chain still verifies; the salt stops anyone from guessing a short erased
//!   text from its hash.
//! - the **signature**, by the author's device key over the room id followed by the header ([`Domain::Event`]).
//!
//! Content is a list of tagged fields (`tag u8 | len u32 | bytes`). A kind this build doesn't know,
//! or a known kind at a higher version, is unknown as a whole and never half-parsed; with the
//! critical flag the fold treats it as a rule it can't apply ([`crate::fold`]).
//!
//! Header layout, little-endian:
//! `format u8 = 1 | kind u16 | kind_version u8 | flags u8 | author [65] | wall_ms u64 |
//!  parent_count u16 | parents [32]* | body_hash [32] | extensions*`.
//! Each extension is `len u32 | bytes`. Only ENTITY v3 and SET v2 allow FLAG_SHAPE,
//! which marks the first extension as their public shape. Later extensions append after it.
//! An unfamiliar flag or extension makes the whole event opaque. Its original bytes still
//! verify, participate in the graph and relay.

use crate::doc::{BreakRef, CharId, TextOp};
use crate::key::{self, DeviceKey, Domain, PublicKey};
use makepad_network::digest::sha256_hash;
use std::io;

pub type Id = [u8; 32];
pub type RoomId = [u8; 32];

pub const FORMAT: u8 = 1;
const FLAG_CRITICAL: u8 = 1;
const FLAG_AI: u8 = 2;
const FLAG_TOOL: u8 = 4;
const FLAG_SHAPE: u8 = 8;
const KNOWN_FLAGS: u8 = FLAG_CRITICAL | FLAG_AI | FLAG_TOOL | FLAG_SHAPE;
const SALT_LEN: usize = 16;

/// Who made an event: the person, or their AI (with the same device key: the flag, set by the
/// Create process, says which).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Actor {
    Human,
    Ai,
}

/// How it was made: in the UI, or by a tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Via {
    Ui,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: u16,
    pub kind_version: u8,
    pub critical: bool,
    pub actor: Actor,
    pub via: Via,
    pub author: PublicKey,
    /// When the author says it was made: shown, never read by the fold.
    pub wall_ms: u64,
    pub parents: Vec<Id>,
    pub body_hash: [u8; 32],
    /// For open ENTITY/SET versions the signed header also commits directly to the public
    /// shape. Erasing the salted body keeps the shape independently verifiable.
    pub shape: Option<Vec<u8>>,
    unknown_flags: u8,
    // Complete length-prefixed frames in their original order.
    unknown_extensions: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// A header format this build doesn't know (a newer Create wrote it).
    NewerFormat(u8),
    Malformed(&'static str),
    BadSignature,
    /// The body doesn't hash to the header's body hash.
    BodyMismatch,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NewerFormat(v) => write!(f, "event format {v} needs a newer Create"),
            FormatError::Malformed(what) => write!(f, "malformed event: {what}"),
            FormatError::BadSignature => f.write_str("the event's signature doesn't verify"),
            FormatError::BodyMismatch => f.write_str("the event's body doesn't match its header"),
        }
    }
}

impl Header {
    pub fn has_unknown_extensions(&self) -> bool {
        self.unknown_flags != 0 || !self.unknown_extensions.is_empty()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(112 + 32 * self.parents.len());
        out.push(FORMAT);
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.push(self.kind_version);
        let mut flags = self.unknown_flags;
        if self.critical {
            flags |= FLAG_CRITICAL;
        }
        if self.actor == Actor::Ai {
            flags |= FLAG_AI;
        }
        if self.via == Via::Tool {
            flags |= FLAG_TOOL;
        }
        if self.shape.is_some() { flags |= FLAG_SHAPE; }
        out.push(flags);
        out.extend_from_slice(&self.author);
        out.extend_from_slice(&self.wall_ms.to_le_bytes());
        // `Event::signed` refuses a draft with more parents than a u16 counts; a decoded header
        // read its count as a u16.
        out.extend_from_slice(&(self.parents.len() as u16).to_le_bytes());
        for parent in &self.parents {
            out.extend_from_slice(parent);
        }
        out.extend_from_slice(&self.body_hash);
        if let Some(shape) = &self.shape {
            // Signing refuses a header over u32::MAX bytes, shape included.
            out.extend_from_slice(&(shape.len() as u32).to_le_bytes());
            out.extend_from_slice(shape);
        }
        out.extend_from_slice(&self.unknown_extensions);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Header, FormatError> {
        let format = *bytes.first().ok_or(FormatError::Malformed("empty header"))?;
        if format != FORMAT {
            return Err(FormatError::NewerFormat(format));
        }
        let mut r = Reader(&bytes[1..]);
        let kind = u16::from_le_bytes(r.take()?);
        let [kind_version] = r.take::<1>()?;
        let [flags] = r.take::<1>()?;
        if flags & FLAG_SHAPE != 0 && !open_version(kind, kind_version) {
            return Err(FormatError::Malformed("public shape on an event without an open form"));
        }
        let author: PublicKey = r.take()?;
        let wall_ms = u64::from_le_bytes(r.take()?);
        let count = u16::from_le_bytes(r.take()?) as usize;
        let mut parents = Vec::with_capacity(count.min(r.0.len() / 32));
        for _ in 0..count {
            parents.push(r.take::<32>()?);
        }
        let body_hash = r.take::<32>()?;
        let shape = if flags & FLAG_SHAPE != 0 { Some(r.extension()?.to_vec()) } else { None };
        let unknown_extensions = r.0;
        while !r.0.is_empty() { r.extension()?; }
        Ok(Header {
            kind,
            kind_version,
            critical: flags & FLAG_CRITICAL != 0,
            actor: if flags & FLAG_AI != 0 { Actor::Ai } else { Actor::Human },
            via: if flags & FLAG_TOOL != 0 { Via::Tool } else { Via::Ui },
            author,
            wall_ms,
            parents,
            body_hash, shape,
            unknown_flags: flags & !KNOWN_FLAGS,
            unknown_extensions: unknown_extensions.to_vec(),
        })
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn extension(&mut self) -> Result<&'a [u8], FormatError> {
        let len = u32::from_le_bytes(self.take()?) as usize;
        let (value, rest) = self.0.split_at_checked(len).ok_or(FormatError::Malformed("truncated header extension"))?;
        self.0 = rest;
        Ok(value)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        let (head, rest) = self.0.split_at_checked(N).ok_or(FormatError::Malformed("header too short"))?;
        self.0 = rest;
        Ok(head.try_into().unwrap())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub(crate) room: RoomId,
    pub id: Id,
    pub header: Header,
    pub header_bytes: Vec<u8>,
    /// Salt and content; empty once redacted.
    pub body: Vec<u8>,
    pub sig: Vec<u8>,
}

/// What a new event says about itself.
pub struct Draft<'a> {
    pub kind: u16,
    pub kind_version: u8,
    pub critical: bool,
    pub actor: Actor,
    pub via: Via,
    pub wall_ms: u64,
    pub parents: Vec<Id>,
    pub content: &'a [u8],
}

impl Event {
    /// Signs a new event; `salt` comes from the OS's random source (a fixed one in tests).
    pub fn create(key: &DeviceKey, room: RoomId, draft: Draft, salt: [u8; SALT_LEN]) -> io::Result<Event> {
        Self::signed(key, Some(room), draft, salt)
    }

    pub fn found(key: &DeviceKey, policy: u16, tier: u8, wall_ms: u64, salt: [u8; SALT_LEN]) -> io::Result<(RoomId, Event)> {
        let content = Content::Room { policy, tier }.encode();
        let draft = Draft { kind: kind::ROOM, kind_version: 1, critical: true, actor: Actor::Human,
            via: Via::Ui, wall_ms, parents: vec![], content: &content };
        let genesis = Self::signed(key, None, draft, salt)?;
        Ok((genesis.room, genesis))
    }

    pub fn genesis_room(header: &[u8]) -> RoomId {
        sha256_hash(&[key::label::ROOM_ID, header].concat())
    }

    fn signed(key: &DeviceKey, room: Option<RoomId>, draft: Draft, salt: [u8; SALT_LEN]) -> io::Result<Event> {
        if draft.kind == kind::ROOM && !matches!(Content::decode(kind::ROOM, draft.kind_version, draft.content),
            Some(Content::Room { tier: 0, .. })) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "this build founds only closed rooms"));
        }
        if draft.parents.len() > u16::MAX as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidInput,
                format!("an event can name at most {} parents, not {}", u16::MAX, draft.parents.len())));
        }
        if draft.content.len() >= u32::MAX as usize - SALT_LEN {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the event's content is too large to store"));
        }
        let mut body = Vec::with_capacity(SALT_LEN + draft.content.len());
        body.extend_from_slice(&salt);
        body.extend_from_slice(draft.content);
        let header = Header {
            kind: draft.kind,
            kind_version: draft.kind_version,
            critical: draft.critical || matches!(draft.kind, kind::ROOM | kind::MEMBER),
            actor: draft.actor,
            via: draft.via,
            author: key.public(),
            wall_ms: draft.wall_ms,
            parents: draft.parents,
            body_hash: sha256_hash(&body),
            shape: content_shape(draft.kind, draft.kind_version, draft.content),
            unknown_flags: 0, unknown_extensions: Vec::new(),
        };
        let header_bytes = header.encode();
        if header_bytes.len() > u32::MAX as usize {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the event's header is too large to store"));
        }
        let room = room.unwrap_or_else(|| Self::genesis_room(&header_bytes));
        if header.kind == kind::ROOM && (!header.parents.is_empty() || Self::genesis_room(&header_bytes) != room) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid room genesis"));
        }
        let sig = key.sign(Domain::Event, &[room.as_slice(), &header_bytes].concat())?;
        Ok(Event { room, id: sha256_hash(&header_bytes), header, header_bytes, body, sig })
    }

    /// An event from its three byte strings as stored or received: the header parses, the
    /// signature verifies, and a body that is present matches the header's hash.
    pub fn from_parts(room: RoomId, header_bytes: Vec<u8>, body: Vec<u8>, sig: Vec<u8>) -> Result<Event, FormatError> {
        let header = Header::decode(&header_bytes)?;
        if matches!(header.kind, kind::ROOM | kind::MEMBER) && !header.critical {
            return Err(FormatError::Malformed("room and member events must be critical"));
        }
        // The fold needs their content: a body-less copy from a peer would hide who may write.
        if matches!(header.kind, kind::ROOM | kind::MEMBER) && body.is_empty() {
            return Err(FormatError::Malformed("room and member events are never redacted"));
        }
        if header.kind == kind::ROOM && (!header.parents.is_empty() || Self::genesis_room(&header_bytes) != room) {
            return Err(FormatError::Malformed("invalid room genesis"));
        }
        if !key::verify(Domain::Event, &header.author, &[room.as_slice(), &header_bytes].concat(), &sig) {
            return Err(FormatError::BadSignature);
        }
        if !body.is_empty() && (body.len() < SALT_LEN || sha256_hash(&body) != header.body_hash) {
            return Err(FormatError::BodyMismatch);
        }
        let event = Event { room, id: sha256_hash(&header_bytes), header, header_bytes, body, sig };
        if let Some(content) = event.content() {
            let header = &event.header;
            if let Some(shape) = content_shape(header.kind, header.kind_version, content) {
                if header.shape.as_ref() != Some(&shape) { return Err(FormatError::BodyMismatch) }
            }
        }
        Ok(event)
    }

    /// Bytes for the content decoder. A redacted body or an opaque header has no
    /// interpretable content. The log always relays the original `body` and `header_bytes`.
    pub fn content(&self) -> Option<&[u8]> {
        if self.header.has_unknown_extensions() { return None }
        self.body.get(SALT_LEN..)
    }

    pub fn redacted(&self) -> bool {
        self.body.is_empty()
    }
}

// ---------------------------------------------------------------- content

pub mod kind {
    /// A member's display name and colour.
    pub const PROFILE: u16 = 1;
    /// A chat message.
    pub const CHAT: u16 = 2;
    // 3 and 4 belonged to the original project and issue events and stay reserved.
    pub const ENTITY: u16 = 5;
    pub const SET: u16 = 6;
    pub const UNDO: u16 = 7;
    pub const TEXT: u16 = 8;
    pub const ROOM: u16 = 9;
    pub const MEMBER: u16 = 10;
}

/// Tagged fields: `tag u8 | len u32 | bytes`, in order; a tag may repeat.
pub fn fields(content: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        let (&tag, tail) = rest.split_first()?;
        let (len, tail) = tail.split_at_checked(4)?;
        let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
        let (value, tail) = tail.split_at_checked(len)?;
        out.push((tag, value));
        rest = tail;
    }
    Some(out)
}

#[derive(Default)]
pub struct FieldWriter(Vec<u8>);

impl FieldWriter {
    pub fn bytes(mut self, tag: u8, value: &[u8]) -> Self {
        self.0.push(tag);
        self.0.extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.0.extend_from_slice(value);
        self
    }
    pub fn text(self, tag: u8, value: &str) -> Self {
        self.bytes(tag, value.as_bytes())
    }
    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// Permanent numeric schema keys and the open Splash namespace. Their wire tags are disjoint.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Number(u16),
    Name(String),
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Number(n) => n.fmt(f), Self::Name(name) => name.fmt(f) }
    }
}

impl Key {
    pub fn number(&self) -> Option<u16> {
        match self { Self::Number(n) => Some(*n), Self::Name(_) => None }
    }

    pub(crate) fn is_order_key(&self) -> bool {
        use crate::entity::field;
        *self == field::ORDER || *self == field::Z
    }

    pub fn accepts(&self, value: &Value) -> bool {
        if self.is_order_key() {
            return match value {
                Value::Key(key) => crate::order_key::valid(key),
                Value::Null => true,
                _ => false,
            }
        }
        match (self, value) {
            (Self::Name(name), Value::Literal(src)) => crate::literal::is_identifier(name) && crate::literal::is_literal(src),
            (Self::Name(_), _) | (_, Value::Literal(_)) => false,
            _ => true,
        }
    }

    fn encode(&self, writer: FieldWriter, number_tag: u8, name_tag: u8) -> FieldWriter {
        match self {
            Self::Number(n) => writer.bytes(number_tag, &n.to_le_bytes()),
            Self::Name(s) => writer.text(name_tag, s),
        }
    }
}

impl PartialEq<u16> for Key {
    fn eq(&self, other: &u16) -> bool { self.number() == Some(*other) }
}

/// The content of the kinds and versions understood by this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Room { policy: u16, tier: u8 },
    Member(Member),
    Profile { name: String, colour: u32 },
    Chat(Chat),
    Entity { kind: Key, project: Option<Id>, derive: Option<(Id, CharId)>, fields: Vec<(Key, Value)> },
    Set { changes: Vec<Change>, label: Option<String> },
    Undo { of: Id },
    Text { doc: Id, ops: Vec<TextOp> },
}

/// Membership operations use one tagged layout. An invite's subject starts with its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Member {
    Invite { subject: PublicKey, role: u8 },
    Accept { entry: Id },
    AddDevice { seat: Id, device: PublicKey },
    Remove { seat: Id, device: PublicKey, last: Id },
}

// Known singular fields must be unambiguous. Unknown tags remain skippable.
fn single_field<'a>(fields: &[(u8, &'a [u8])], tag: u8) -> Option<&'a [u8]> {
    let mut matches = fields.iter().filter(|f| f.0 == tag);
    let bytes = matches.next()?.1;
    matches.next().is_none().then_some(bytes)
}

impl Member {
    fn encode(&self) -> Vec<u8> {
        // Tags: operation, subject, role, entry, seat, device, last.
        let op = |n| FieldWriter::default().bytes(1, &[n]);
        match self {
            Self::Invite { subject, role } => op(1).bytes(2, &[&[1][..], subject].concat()).bytes(3, &[*role]),
            Self::Accept { entry } => op(2).bytes(4, entry),
            Self::AddDevice { seat, device } => op(3).bytes(5, seat).bytes(6, device),
            Self::Remove { seat, device, last } => op(4).bytes(5, seat).bytes(6, device).bytes(7, last),
        }.finish()
    }

    fn decode(fields: &[(u8, &[u8])]) -> Option<Self> {
        let get = |tag| single_field(fields, tag);
        let point = |bytes: &[u8]| -> Option<PublicKey> {
            if bytes.first() != Some(&4) { return None }
            bytes.try_into().ok()
        };
        Some(match get(1)? {
            [1] => Self::Invite { subject: point(get(2)?.strip_prefix(&[1])?)?, role: u8::from_le_bytes(get(3)?.try_into().ok()?) },
            [2] => Self::Accept { entry: get(4)?.try_into().ok()? },
            [3] => Self::AddDevice { seat: get(5)?.try_into().ok()?, device: point(get(6)?)? },
            [4] => Self::Remove { seat: get(5)?.try_into().ok()?, device: point(get(6)?)?, last: get(7)?.try_into().ok()? },
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub entity: Id,
    pub field: Key,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Text(String),
    Id(Id),
    Key(Vec<u8>),
    Blob { size: u64, root: [u8; 32] },
    Anchor(Anchor),
    Literal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// Board coordinates in 1/64 canvas units.
    Spot { x: i64, y: i64 },
    Time { ms: u64 },
    Text { run: Id, index: u32 },
}

impl Anchor {
    fn encode(&self) -> Vec<u8> {
        let w = FieldWriter::default();
        match self {
            Self::Spot { x, y } => w.bytes(1, &[x.to_le_bytes(), y.to_le_bytes()].concat()),
            Self::Time { ms } => w.bytes(2, &ms.to_le_bytes()),
            Self::Text { run, index } => {
                let mut bytes = run.to_vec();
                bytes.extend(index.to_le_bytes());
                w.bytes(3, &bytes)
            }
        }.finish()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        fields(bytes)?.into_iter().find_map(|(tag, data)| match tag {
            1 if data.len() == 16 => Some(Self::Spot {
                x: i64::from_le_bytes(data[..8].try_into().ok()?),
                y: i64::from_le_bytes(data[8..].try_into().ok()?),
            }),
            2 => Some(Self::Time { ms: u64::from_le_bytes(data.try_into().ok()?) }),
            3 if data.len() == 36 => Some(Self::Text {
                run: data[..32].try_into().ok()?,
                index: u32::from_le_bytes(data[32..].try_into().ok()?),
            }),
            _ => None,
        })
    }
}

// Each initial field or change has its own TLV envelope: number (1) or name (11), target
// (2, SET only), and one value (3..10 or literal source at 12). Names and literals are
// interpreted only in ENTITY v3 and SET v2. Legacy decoders keep their skip-one-entry rule.
fn encode_field(field: &Key, entity: Option<&Id>, value: &Value) -> Vec<u8> {
    let mut w = field.encode(FieldWriter::default(), 1, 11);
    if let Some(entity) = entity {
        w = w.bytes(2, entity);
    }
    match value {
        Value::Null => w.bytes(3, &[]),
        Value::Bool(b) => w.bytes(4, &[u8::from(*b)]),
        Value::Int(n) => w.bytes(5, &n.to_le_bytes()),
        Value::Text(s) => w.text(6, s),
        Value::Id(id) => w.bytes(7, id),
        Value::Key(key) => w.bytes(8, key),
        Value::Blob { size, root } => {
            let mut data = [0; 40];
            data[..8].copy_from_slice(&size.to_be_bytes());
            data[8..].copy_from_slice(root);
            w.bytes(9, &data)
        }
        Value::Anchor(a) => w.bytes(10, &a.encode()),
        Value::Literal(src) => w.text(12, src),
    }.finish()
}

fn decode_key(fields: &[(u8, &[u8])], number: u8, name: u8, open: bool) -> Option<Key> {
    if open {
        if let Some((_, bytes)) = fields.iter().find(|f| f.0 == name) {
            if fields.iter().any(|f| f.0 == number) { return None }
            return Some(Key::Name(std::str::from_utf8(bytes).ok()?.into()))
        }
    }
    Some(Key::Number(u16::from_le_bytes(fields.iter().find(|f| f.0 == number)?.1.try_into().ok()?)))
}

fn decode_field(bytes: &[u8], open: bool) -> Option<(Key, Option<Id>, Value)> {
    let entry = fields(bytes)?;
    if open {
        let keys = entry.iter().filter(|f| matches!(f.0, 1 | 11)).count();
        let values = entry.iter().filter(|f| !matches!(f.0, 1 | 2 | 11)).count();
        if keys != 1 || values != 1 { return None }
    }
    let field = decode_key(&entry, 1, 11, open)?;
    let entity = entry.iter().find(|f| f.0 == 2).and_then(|f| f.1.try_into().ok());
    let value = entry.into_iter().find_map(|(tag, data)| Some(match tag {
        3 if data.is_empty() => Value::Null,
        4 if data == [0] || data == [1] => Value::Bool(data[0] == 1),
        5 => Value::Int(i64::from_le_bytes(data.try_into().ok()?)),
        6 => Value::Text(std::str::from_utf8(data).ok()?.into()),
        7 => Value::Id(data.try_into().ok()?),
        8 => Value::Key(data.to_vec()),
        9 => {
            let (size, root) = data.split_first_chunk::<8>()?;
            Value::Blob { size: u64::from_be_bytes(*size), root: root.try_into().ok()? }
        }
        10 => Value::Anchor(Anchor::decode(data)?),
        12 if open => Value::Literal(std::str::from_utf8(data).ok()?.into()),
        _ => return None,
    }))?;
    Some((field, entity, value))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chat {
    pub text: String,
    /// Addressed to the author's own AI (the `@`): a field set by the composer, never parsed
    /// from the text. It counts only on the author's own human events ([`crate::fold::ai_visible`]).
    pub to_ai: bool,
    /// A message this one quotes (one level: a quoted message's own quote brings nothing).
    pub quote: Option<Id>,
    /// Tasks it refers to (reference chips), by the id of the event that created each.
    pub tasks: Vec<Id>,
}

fn open_version(kind: u16, version: u8) -> bool {
    matches!((kind, version), (kind::ENTITY, 3) | (kind::SET, 2))
}

// A projection of the actual signed write, never a skeleton supplied later by a redactor.
// Use the same key tags as the content. Values, labels and the salt occur only in the body.
fn content_shape(kind: u16, version: u8, bytes: &[u8]) -> Option<Vec<u8>> {
    if !open_version(kind, version) { return None }
    let mut shape = FieldWriter::default();
    match Content::decode(kind, version, bytes)? {
        Content::Entity { kind, project, derive, fields } => {
            shape = kind.encode(shape, 1, 5);
            if let Some(id) = project { shape = shape.bytes(2, &id); }
            if let Some((doc, at)) = derive {
                shape = shape.bytes(4, &[doc.as_slice(), &at.bytes()].concat());
            }
            for (key, _) in fields {
                shape = shape.bytes(3, &key.encode(FieldWriter::default(), 1, 11).finish());
            }
        }
        Content::Set { changes, .. } => {
            for change in changes {
                let key = change.field.encode(FieldWriter::default(), 1, 11);
                shape = shape.bytes(1, &key.bytes(2, &change.entity).finish());
            }
        }
        _ => return None,
    }
    Some(shape.finish())
}

impl Content {
    pub fn kind(&self) -> u16 {
        match self {
            Content::Member(_) => kind::MEMBER,
            Content::Room { .. } => kind::ROOM,
            Content::Profile { .. } => kind::PROFILE,
            Content::Chat(_) => kind::CHAT,
            Content::Entity { .. } => kind::ENTITY,
            Content::Set { .. } => kind::SET,
            Content::Text { .. } => kind::TEXT,
            Content::Undo { .. } => kind::UNDO,
        }
    }

    pub fn version(&self) -> u8 {
        // A v1 reader must never assign the signing event's id to a derived task.
        let extended = |key: &Key, value: &Value| matches!(key, Key::Name(_)) || matches!(value, Value::Literal(_));
        match self {
            Self::Entity { kind, fields, .. } if matches!(kind, Key::Name(_)) || fields.iter().any(|(k, v)| extended(k, v)) => 3,
            Self::Entity { derive: Some(_), .. } => 2,
            Self::Set { changes, .. } if changes.iter().any(|c| extended(&c.field, &c.value)) => 2,
            _ => 1,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Content::Member(op) => op.encode(),
            Content::Room { policy, tier } => FieldWriter::default().bytes(1, &policy.to_le_bytes()).bytes(2, &[*tier]).finish(),
            Content::Profile { name, colour } => FieldWriter::default().text(1, name).bytes(2, &colour.to_le_bytes()).finish(),
            Content::Chat(chat) => {
                let mut w = FieldWriter::default().text(1, &chat.text);
                if chat.to_ai {
                    w = w.bytes(2, &[]);
                }
                if let Some(quote) = &chat.quote {
                    w = w.bytes(3, quote);
                }
                for task in &chat.tasks {
                    w = w.bytes(4, task);
                }
                w.finish()
            }
            Content::Entity { kind, project, derive, fields } => {
                let mut w = kind.encode(FieldWriter::default(), 1, 5);
                if let Some(project) = project {
                    w = w.bytes(2, project);
                }
                if let Some((doc, at)) = derive {
                    w = w.bytes(4, &[doc.as_slice(), &at.bytes()].concat());
                }
                for (field, value) in fields {
                    w = w.bytes(3, &encode_field(field, None, value));
                }
                w.finish()
            }
            Content::Set { changes, label } => {
                let mut w = FieldWriter::default();
                for change in changes {
                    w = w.bytes(1, &encode_field(&change.field, Some(&change.entity), &change.value));
                }
                if let Some(label) = label {
                    w = w.text(2, label);
                }
                w.finish()
            }
            Content::Text { doc, ops } => {
                let mut w = FieldWriter::default().bytes(1, doc);
                for op in ops {
                    let entry = FieldWriter::default();
                    let (tag, entry) = match op {
                        TextOp::Insert { anchor, text } => (2, entry.bytes(1, &anchor.bytes()).text(2, text)),
                        TextOp::Delete { from, to } => (3, entry.bytes(1, &from.bytes()).bytes(2, &to.bytes())),
                        TextOp::Attr { at, attr, value } => (4, entry.bytes(1, &at.bytes()).bytes(2, &encode_field(&Key::Number(*attr), None, value))),
                        TextOp::Move { block, after } => (5, entry.bytes(1, &block.bytes()).bytes(2, &after.bytes())),
                    };
                    w = w.bytes(tag, &entry.finish());
                }
                w.finish()
            }
            Content::Undo { of } => FieldWriter::default().bytes(1, of).finish(),
        }
    }

    /// The content of a known kind and version; `None` for anything else (an unknown kind, a newer
    /// version, or malformed fields). Unknown tags inside a known kind are skipped: a field added
    /// later that an older build may ignore gets a new tag, one it must understand a new version.
    pub fn decode(kind: u16, version: u8, content: &[u8]) -> Option<Content> {
        let open = open_version(kind, version);
        if !open && version != 1 && !(kind == kind::ENTITY && version == 2) {
            return None;
        }
        let fields = fields(content)?;
        let text = |tag: u8| -> Option<String> {
            match fields.iter().find(|f| f.0 == tag) {
                Some((_, v)) => String::from_utf8(v.to_vec()).ok(),
                None => Some(String::new()),
            }
        };
        let id = |value: &[u8]| -> Option<Id> { value.try_into().ok() };
        Some(match kind {
            kind::MEMBER => Content::Member(Member::decode(&fields)?),
            kind::ROOM => Content::Room {
                policy: u16::from_le_bytes(single_field(&fields, 1)?.try_into().ok()?),
                tier: u8::from_le_bytes(single_field(&fields, 2)?.try_into().ok()?),
            },
            kind::PROFILE => {
                let colour = match fields.iter().find(|f| f.0 == 2) {
                    Some((_, v)) => u32::from_le_bytes((*v).try_into().ok()?),
                    None => 0,
                };
                Content::Profile { name: text(1)?, colour }
            }
            kind::CHAT => {
                let mut chat = Chat { text: text(1)?, ..Chat::default() };
                for (tag, value) in &fields {
                    match tag {
                        2 => chat.to_ai = true,
                        3 => chat.quote = Some(id(value)?),
                        4 => chat.tasks.push(id(value)?),
                        _ => {}
                    }
                }
                Content::Chat(chat)
            }
            kind::ENTITY => {
                let kind = decode_key(&fields, 1, 5, open)?;
                let project = match fields.iter().find(|f| f.0 == 2) {
                    Some((_, value)) => Some(id(value)?),
                    None => None,
                };
                let derive = if version == 2 || (open && fields.iter().any(|f| f.0 == 4)) {
                    let (doc, at) = fields.iter().find(|f| f.0 == 4)?.1.split_at_checked(32)?;
                    Some((id(doc)?, CharId::decode(at)?))
                } else { None };
                let entries = fields.iter().filter(|f| f.0 == 3).map(|f| {
                    let (key, _, value) = decode_field(f.1, open)?;
                    Some((key, value))
                });
                let fields = if open { entries.collect::<Option<_>>()? } else { entries.flatten().collect() };
                Content::Entity { kind, project, derive, fields }
            }
            kind::SET => {
                let label = match fields.iter().find(|f| f.0 == 2) {
                    Some((_, bytes)) => Some(std::str::from_utf8(bytes).ok()?.into()),
                    None => None,
                };
                let entries = fields.iter().filter(|f| f.0 == 1).map(|f| {
                    let (field, entity, value) = decode_field(f.1, open)?;
                    Some(Change { entity: entity?, field, value })
                });
                let changes = if open { entries.collect::<Option<_>>()? } else { entries.flatten().collect() };
                Content::Set { changes, label }
            }
            kind::TEXT => {
                let doc = id(fields.iter().find(|f| f.0 == 1)?.1)?;
                let mut ops = Vec::new();
                for (tag, bytes) in fields {
                    if !(2..=5).contains(&tag) { continue }
                    let entry = self::fields(bytes)?;
                    let get = |tag| entry.iter().find(|f| f.0 == tag).map(|f| f.1);
                    let a = get(1)?;
                    let b = get(2)?;
                    ops.push(match tag {
                        2 => TextOp::Insert { anchor: BreakRef::decode(a)?, text: std::str::from_utf8(b).ok()?.into() },
                        3 => TextOp::Delete { from: CharId::decode(a)?, to: CharId::decode(b)? },
                        4 => {
                            let (attr, _, value) = decode_field(b, false)?;
                            TextOp::Attr { at: BreakRef::decode(a)?, attr: attr.number()?, value }
                        }
                        _ => TextOp::Move { block: BreakRef::decode(a)?, after: BreakRef::decode(b)? },
                    });
                }
                Content::Text { doc, ops }
            }
            kind::UNDO => Content::Undo { of: id(fields.iter().find(|f| f.0 == 1)?.1)? },
            _ => return None,
        })
    }
}

use crate::fold::cache::Cached;
impl Cached for Key {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            Self::Number(n) => { out.push(0); n.save(out); }
            Self::Name(s) => { out.push(1); s.save(out); }
        }
    }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match u8::load(input)? { 0 => Some(Self::Number(u16::load(input)?)), 1 => Some(Self::Name(String::load(input)?)), _ => None }
    }
}
impl Cached for Value {
    fn save(&self, out: &mut Vec<u8>) { encode_field(&Key::Number(0), None, self).save(out); }
    fn load(input: &mut &[u8]) -> Option<Self> { Some(decode_field(&Vec::<u8>::load(input)?, true)?.2) }
}
impl Cached for Actor {
    fn save(&self, out: &mut Vec<u8>) { out.push(u8::from(*self == Self::Ai)); }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match u8::load(input)? { 0 => Some(Self::Human), 1 => Some(Self::Ai), _ => None }
    }
}
impl Cached for Via {
    fn save(&self, out: &mut Vec<u8>) { out.push(u8::from(*self == Self::Tool)); }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match u8::load(input)? { 0 => Some(Self::Ui), 1 => Some(Self::Tool), _ => None }
    }
}
impl Cached for Chat {
    fn save(&self, out: &mut Vec<u8>) { Content::Chat(self.clone()).encode().save(out); }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match Content::decode(kind::CHAT, 1, &Vec::<u8>::load(input)?)? { Content::Chat(chat) => Some(chat), _ => None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_round_trips_without_losing_types_or_list_order() {
        let values = vec![
            Value::Null, Value::Bool(false), Value::Bool(true), Value::Int(i64::MIN),
            Value::Text("two 🗒".into()), Value::Id([8; 32]), Value::Key(vec![1, 128]),
            Value::Blob { size: 1234, root: [8; 32] }, Value::Anchor(Anchor::Spot { x: -64, y: i64::MAX }),
            Value::Anchor(Anchor::Time { ms: u64::MAX }),
            Value::Anchor(Anchor::Text { run: [9; 32], index: u32::MAX }),
        ];
        let fields: Vec<_> = values.into_iter().enumerate().map(|(n, v)| (Key::Number(n as u16), v)).collect();
        let changes = fields.iter().map(|(field, value)| Change { entity: [1; 32], field: field.clone(), value: value.clone() }).collect();
        for content in [
            Content::Profile { name: "Ada".into(), colour: 42 }, Content::Chat(Chat::default()),
            Content::Entity { kind: Key::Number(1234), derive: None, project: None, fields: fields.clone() },
            Content::Entity { kind: Key::Number(4567), derive: Some(([3; 32], CharId { event: [4; 32], index: 7 })), project: Some([2; 32]), fields },
            Content::Set { changes, label: Some("tidied this board".into()) },
            Content::Set { changes: vec![], label: None }, Content::Undo { of: [3; 32] },
        ] {
            assert_eq!(Content::decode(content.kind(), content.version(), &content.encode()).as_ref(), Some(&content), "round trip for {content:?}");
        }
        for reserved in [3, 4] {
            assert_eq!(Content::decode(reserved, 1, &[]), None, "kind {reserved} stays reserved");
        }
    }

    #[test]
    fn an_unknown_value_skips_exactly_one_change() {
        let entity = [1; 32];
        let unknown = FieldWriter::default().bytes(1, &3u16.to_le_bytes()).bytes(2, &entity).bytes(200, &[0xff]).finish();
        let bytes = FieldWriter::default()
            .bytes(1, &encode_field(&Key::Number(2), Some(&entity), &Value::Int(6)))
            .bytes(1, &unknown)
            .bytes(1, &encode_field(&Key::Number(4), Some(&entity), &Value::Null))
            .finish();
        assert_eq!(Content::decode(kind::SET, 1, &bytes), Some(Content::Set {
            changes: vec![Change { entity, field: Key::Number(2), value: Value::Int(6) }, Change { entity, field: Key::Number(4), value: Value::Null }],
            label: None,
        }), "known changes on both sides survive an unknown value");
    }

    #[test]
    fn erased_open_changes_keep_only_authenticated_shape() {
        let key = DeviceKey::generate().unwrap();
        let (room, _) = Event::found(&key, 1, 0, 0, [0; 16]).unwrap();
        let source = Value::Literal("Private{secret: 1234}".into());
        let name = Key::Name("diagram".into());
        for content in [
            Content::Entity { kind: Key::Name("FutureCard".into()), project: None, derive: None,
                fields: vec![(name.clone(), source.clone())] },
            Content::Set { changes: vec![Change { entity: [1; 32], field: name, value: source }],
                label: Some("private group".into()) },
        ] {
            let bytes = content.encode();
            let event = Event::create(&key, room, Draft { kind: content.kind(), kind_version: content.version(),
                ..draft(&bytes, vec![]) }, [33; 16]).unwrap();
            let read = Event::from_parts(room, event.header_bytes.clone(), event.body.clone(), event.sig.clone()).unwrap();
            assert_eq!(Content::decode(read.header.kind, read.header.kind_version, read.content().unwrap()), Some(content));
            let erased = Event::from_parts(room, event.header_bytes.clone(), vec![], event.sig.clone()).unwrap();
            assert_eq!(erased.id, event.id);
            assert!(erased.content().is_none());
            let shape = erased.header.shape.unwrap();
            assert!(shape.windows(7).any(|s| s == b"diagram"));
            assert!(!shape.windows(6).any(|s| s == b"secret"));
            assert!(!shape.windows(7).any(|s| s == b"private"));
            let mut changed = event.header_bytes;
            *changed.last_mut().unwrap() ^= 1;
            assert_eq!(Event::from_parts(room, changed, vec![], event.sig), Err(FormatError::BadSignature));
        }
        let entity = [1; 32];
        let mut future = FieldWriter::default();
        for field in [1, 2] {
            future = future.bytes(1, &encode_field(&Key::Number(field), Some(&entity), &Value::Bool(true)));
        }
        let unknown = Key::Name("p".into()).encode(FieldWriter::default(), 1, 11).bytes(2, &entity).bytes(99, b"?").finish();
        assert_eq!(Content::decode(kind::SET, 2, &future.bytes(1, &unknown).finish()), None,
            "an unreadable entry in the extended version hides the entire group");
    }

    pub(crate) fn draft(content: &[u8], parents: Vec<Id>) -> Draft<'_> {
        Draft { kind: kind::CHAT, kind_version: 1, critical: false, actor: Actor::Human, via: Via::Ui, wall_ms: 1_700_000_000_000, parents, content }
    }

    #[test]
    fn events_sign_hash_and_refuse_tampering() {
        let key = DeviceKey::generate().unwrap();
        let (room, genesis) = Event::found(&key, 1, 0, 0, [0; 16]).unwrap();
        let other = DeviceKey::generate().unwrap();
        let (other_room, _) = Event::found(&other, 1, 0, 0, [0; 16]).unwrap();
        assert_ne!(room, other_room, "the founder contributes to the room id");
        assert_eq!(Event::from_parts(room, genesis.header_bytes.clone(), vec![], genesis.sig.clone()),
            Err(FormatError::Malformed("room and member events are never redacted")), "a body-less genesis is refused");
        let wrong = key.sign(Domain::Event, &[other_room.as_slice(), &genesis.header_bytes].concat()).unwrap();
        assert_eq!(Event::from_parts(room, genesis.header_bytes, genesis.body, wrong), Err(FormatError::BadSignature),
            "genesis must be signed in its own room");
        let add = Content::Member(Member::AddDevice { seat: [3; 32], device: other.public() });
        let mut extended = add.encode();
        extended.extend(FieldWriter::default().bytes(80, b"future certificate").finish());
        assert_eq!(Content::decode(kind::MEMBER, 1, &extended), Some(add), "an unknown add_device tag is skippable");
        let chat = Content::Chat(Chat { text: "hello".into(), to_ai: true, quote: Some([3; 32]), tasks: vec![[4; 32], [5; 32]] });
        let content = chat.encode();
        let event = Event::create(&key, room, Draft { parents: vec![[9; 32]], ..draft(&content, vec![]) }, [1; 16]).unwrap();
        assert_eq!(Content::decode(event.header.kind, event.header.kind_version, event.content().unwrap()), Some(chat), "content round-trips");
        assert_eq!(event.header.author, key.public(), "the author is the signing key");
        assert_eq!(Header::decode(&event.header_bytes).as_ref(), Ok(&event.header), "header round-trips");

        let again = Event::from_parts(room, event.header_bytes.clone(), event.body.clone(), event.sig.clone()).unwrap();
        assert_eq!(again, event, "an event read back is the same event");
        assert_eq!(Event::from_parts(other_room, event.header_bytes.clone(), event.body.clone(), event.sig.clone()),
            Err(FormatError::BadSignature), "a valid event cannot cross rooms");
        let resigned = key.sign(Domain::Event, &[room.as_slice(), &event.header_bytes].concat()).unwrap();
        let other_sig = Event::from_parts(room, event.header_bytes.clone(), event.body.clone(), resigned).unwrap();
        assert_eq!(other_sig.id, event.id, "the id never covers the signature");

        let redacted = Event::from_parts(room, event.header_bytes.clone(), Vec::new(), event.sig.clone()).unwrap();
        assert!(redacted.redacted() && redacted.content().is_none() && redacted.id == event.id, "an erased body keeps the event");

        let mut shaped = event.header.clone();
        shaped.shape = Some(FieldWriter::default().text(11, "invented").finish());
        for (kind, version) in [(kind::CHAT, 1), (kind::ENTITY, 1), (kind::ENTITY, 2), (kind::ENTITY, 4),
                                (kind::SET, 1), (kind::SET, 3), (99, 2)] {
            shaped.kind = kind;
            shaped.kind_version = version;
            let bytes = shaped.encode();
            let sig = key.sign(Domain::Event, &[room.as_slice(), &bytes].concat()).unwrap();
            for body in [event.body.clone(), Vec::new()] {
                assert!(matches!(Event::from_parts(room, bytes.clone(), body, sig.clone()), Err(FormatError::Malformed(_))),
                    "a signed shape on kind {kind} v{version} is malformed even after redaction");
            }
        }

        let mut header = event.header_bytes.clone();
        header[80] ^= 1;
        assert_eq!(Event::from_parts(room, header, event.body.clone(), event.sig.clone()), Err(FormatError::BadSignature), "a changed header");
        let mut body = event.body.clone();
        *body.last_mut().unwrap() ^= 1;
        assert_eq!(Event::from_parts(room, event.header_bytes.clone(), body, event.sig.clone()), Err(FormatError::BodyMismatch), "a changed body");
        let other = DeviceKey::generate().unwrap();
        let forged = other.sign(Domain::Event, &[room.as_slice(), &event.header_bytes].concat()).unwrap();
        assert_eq!(Event::from_parts(room, event.header_bytes.clone(), event.body.clone(), forged), Err(FormatError::BadSignature), "someone else's signature");
        let mut newer = event.header_bytes.clone();
        newer[0] = 2;
        assert_eq!(Event::from_parts(room, newer, Vec::new(), event.sig.clone()), Err(FormatError::NewerFormat(2)), "a newer format is named");
        for end in 0..event.header_bytes.len() {
            assert!(Header::decode(&event.header_bytes[..end]).is_err(), "a header cut at {end}");
        }

        assert_eq!(Content::decode(99, 1, &content), None, "an unknown kind is unknown as a whole");
        assert_eq!(Content::decode(kind::CHAT, 2, &content), None, "a newer version is unknown as a whole");
        assert_eq!(Content::decode(kind::CHAT, 1, &content[..content.len() - 1]), None, "cut fields are refused");
        let with_new_tag = FieldWriter::default().text(1, "hi").text(77, "later").finish();
        assert!(matches!(Content::decode(kind::CHAT, 1, &with_new_tag), Some(Content::Chat(c)) if c.text == "hi"), "an unknown tag is skipped");
    }
}
