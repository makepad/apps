//! The event format, v1. An event is three byte strings, kept exactly as written or received:
//!
//! - the **header**, signed and hashed: format, kind and its version, flags (critical, actor,
//!   via, public shape), the author's public key, the wall time (display only), the parents and the hash of the
//!   body. The event's id is SHA-256 of the header alone: ECDSA signatures aren't unique, and an id
//!   that covered one would let anyone mint "new" events from old ones by signing again.
//! - the **body**: 16 random bytes of salt, then the content. Redaction erases the body and keeps
//!   the header, so the chain still verifies; the salt stops anyone from guessing a short erased
//!   text from its hash.
//! - the **signature**, by the author's device key over the town id followed by the header ([`Domain::Event`]).
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
pub type TownId = [u8; 32];
pub type SessionId = [u8; 16];

pub const FORMAT: u8 = 1;
const FLAG_CRITICAL: u8 = 1;
const FLAG_AI: u8 = 2;
const FLAG_TOOL: u8 = 4;
const FLAG_SHAPE: u8 = 8;
const KNOWN_FLAGS: u8 = FLAG_CRITICAL | FLAG_AI | FLAG_TOOL | FLAG_SHAPE;
const SALT_LEN: usize = 16;

/// Who made an event: the person, or their AI (with the same device key: the flag, set by the
/// Town process, says which).
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
    /// A header format this build doesn't know (a newer Town wrote it).
    NewerFormat(u8),
    Malformed(&'static str),
    BadSignature,
    /// The body doesn't hash to the header's body hash.
    BodyMismatch,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NewerFormat(v) => write!(f, "event format {v} needs a newer Town"),
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
        let author: PublicKey = r.take()?;
        let wall_ms = u64::from_le_bytes(r.take()?);
        let count = u16::from_le_bytes(r.take()?) as usize;
        let mut parents = Vec::with_capacity(count.min(r.0.len() / 32));
        for _ in 0..count {
            parents.push(r.take::<32>()?);
        }
        let body_hash = r.take::<32>()?;
        let mut shape = None;
        let mut unknown_flags = flags & !KNOWN_FLAGS;
        let mut unknown_extensions = r.0;
        if flags & FLAG_SHAPE != 0 {
            match ContentForm::for_version(kind, kind_version) {
                Some(form) if !form.is_open() => return Err(FormatError::Malformed("public shape on an event without an open form")),
                Some(_) => {
                    shape = Some(r.extension()?.to_vec());
                    unknown_extensions = r.0;
                }
                None => {
                    // Keep the required frame, including its length, with the opaque extensions.
                    r.extension()?;
                    unknown_flags |= FLAG_SHAPE;
                }
            }
        }
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
            unknown_flags,
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
    pub(crate) town: TownId,
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
    pub fn create(key: &DeviceKey, town: TownId, draft: Draft, salt: [u8; SALT_LEN]) -> io::Result<Event> {
        Self::signed(key, Some(town), draft, salt)
    }

    pub fn found(key: &DeviceKey, policy: u16, tier: u8, wall_ms: u64, salt: [u8; SALT_LEN]) -> io::Result<(TownId, Event)> {
        let content = Content::Town { policy, tier }.encode();
        let draft = Draft { kind: kind::TOWN, kind_version: 1, critical: true, actor: Actor::Human,
            via: Via::Ui, wall_ms, parents: vec![], content: &content };
        let genesis = Self::signed(key, None, draft, salt)?;
        Ok((genesis.town, genesis))
    }

    pub fn genesis_town(header: &[u8]) -> TownId {
        sha256_hash(&[key::label::TOWN_ID, header].concat())
    }

    fn signed(key: &DeviceKey, town: Option<TownId>, draft: Draft, salt: [u8; SALT_LEN]) -> io::Result<Event> {
        if draft.kind == kind::TOWN && !matches!(Content::decode(kind::TOWN, draft.kind_version, draft.content),
            Some(Content::Town { tier: 0, .. })) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "this build founds only closed towns"));
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
            critical: draft.critical || matches!(draft.kind, kind::TOWN | kind::MEMBER),
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
        let town = town.unwrap_or_else(|| Self::genesis_town(&header_bytes));
        if header.kind == kind::TOWN && (!header.parents.is_empty() || Self::genesis_town(&header_bytes) != town) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid town genesis"));
        }
        let sig = key.sign(Domain::Event, &[town.as_slice(), &header_bytes].concat())?;
        Ok(Event { town, id: sha256_hash(&header_bytes), header, header_bytes, body, sig })
    }

    /// An event from its three byte strings as stored or received: the header parses, the
    /// signature verifies, and a body that is present matches the header's hash.
    pub fn from_parts(town: TownId, header_bytes: Vec<u8>, body: Vec<u8>, sig: Vec<u8>) -> Result<Event, FormatError> {
        let header = Header::decode(&header_bytes)?;
        if matches!(header.kind, kind::TOWN | kind::MEMBER) && !header.critical {
            return Err(FormatError::Malformed("town and member events must be critical"));
        }
        // The fold needs their content: a body-less copy from a peer would hide who may write.
        if matches!(header.kind, kind::TOWN | kind::MEMBER) && body.is_empty() {
            return Err(FormatError::Malformed("town and member events are never redacted"));
        }
        if header.kind == kind::TOWN && (!header.parents.is_empty() || Self::genesis_town(&header_bytes) != town) {
            return Err(FormatError::Malformed("invalid town genesis"));
        }
        if !key::verify(Domain::Event, &header.author, &[town.as_slice(), &header_bytes].concat(), &sig) {
            return Err(FormatError::BadSignature);
        }
        if !body.is_empty() && (body.len() < SALT_LEN || sha256_hash(&body) != header.body_hash) {
            return Err(FormatError::BodyMismatch);
        }
        let event = Event { town, id: sha256_hash(&header_bytes), header, header_bytes, body, sig };
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
    pub const TOWN: u16 = 9;
    pub const MEMBER: u16 = 10;
    pub const AGENT: u16 = 11;
    pub const SESSION: u16 = 12;
    pub const SAID: u16 = 13;
    pub const SERVICE: u16 = 14;
    /// A person takes back one of their own chat messages.
    pub const RETRACT: u16 = 15;
    /// Reserved for intake clearance.
    pub const INTAKE: u16 = 16;
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
            (Self::Number(40 | 42..=44), _) => matches!(value, Value::Text(_) | Value::Null),
            (Self::Number(41), _) => matches!(value, Value::Int(_) | Value::Null),
            (Self::Number(45..=47), _) => false,
            (Self::Number(48), _) => matches!(value, Value::Int(_)),
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
    Town { policy: u16, tier: u8 },
    Member(Member),
    Profile { name: String, colour: u32 },
    Chat(Chat),
    /// The same chat v1 layout. Older readers skip the named-agent tag.
    AgentChat { chat: Chat, agent: ChatAgent },
    AgentAdded { name: String, kind: String },
    AgentRemoved { agent: Id },
    AgentRenamed { agent: Id, name: String },
    Session(SessionOp),
    Said { session: SessionId, lines: Vec<SaidLine> },
    Provides { services: Vec<Service> },
    Request { job: JobId, service: Service, session: SessionId, project: Option<Id>, provider: PublicKey },
    Taken { job: JobId },
    Done { job: JobId, text: String },
    Failed { job: JobId, why: Failure },
    Shared { session: SessionId, agent: Id, line: Id },
    Entity { kind: Key, project: Option<Id>, derive: Option<(Id, CharId)>, fields: Vec<(Key, Value)>, copied_from: Vec<CopyRef> },
    Set { changes: Vec<Change>, label: Option<String>, copied_from: Vec<CopyRef> },
    Undo { of: Id },
    Retract { of: Id },
    Text { doc: Id, ops: Vec<TextOp> },
}

/// Repeated top-level ENTITY/SET tag for copy-by-code provenance.
pub const TAG_COPIED_FROM: u8 = 47;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CopyRef {
    /// Implicit for a creation (including a derived task), explicit for a set.
    pub target_entity: Option<Id>,
    pub target_field: Key,
    pub sources: Vec<CopySource>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CopySource {
    Record(Id),
    /// The source field's authoring event at the time of the copy.
    EntityField { entity: Id, field: Key, writer: Id },
    DocLine { doc: Id, events: Vec<Id> },
    SharePeer { peer: PublicKey },
}

/// Canonicalize before signing or indexing. The wire codec preserves the signed order.
pub fn normalize_copy_refs(refs: &mut Vec<CopyRef>) {
    let mut targets = std::collections::BTreeMap::new();
    for mut reference in refs.drain(..) {
        if reference.sources.is_empty() { continue }
        for source in &mut reference.sources {
            if let CopySource::DocLine { events, .. } = source {
                events.sort_unstable();
                events.dedup();
            }
        }
        targets.entry((reference.target_entity, reference.target_field))
            .or_insert_with(Vec::new).append(&mut reference.sources);
    }
    refs.extend(targets.into_iter().map(|((target_entity, target_field), mut sources)| {
        sources.sort_unstable();
        sources.dedup();
        CopyRef { target_entity, target_field, sources }
    }));
}

// Provenance keys require exactly one representation, including in numeric-only versions.
fn copy_key(number: Option<&[u8]>, name: Option<&[u8]>, open: bool) -> Option<Key> {
    match (number, name) {
        (Some(n), None) => Some(Key::Number(u16::from_le_bytes(n.try_into().ok()?))),
        (None, Some(s)) if open => Some(Key::Name(std::str::from_utf8(s).ok()?.into())),
        _ => None,
    }
}

impl CopySource {
    fn encode(&self) -> Vec<u8> {
        let w = FieldWriter::default();
        match self {
            Self::Record(id) => w.bytes(1, &[1]).bytes(2, id),
            Self::EntityField { entity, field, writer } =>
                field.encode(w.bytes(1, &[2]).bytes(2, entity), 3, 4).bytes(5, writer),
            Self::DocLine { doc, events } => {
                let mut w = w.bytes(1, &[3]).bytes(2, doc);
                for event in events { w = w.bytes(6, event); }
                w
            }
            Self::SharePeer { peer } => w.bytes(1, &[4]).bytes(2, peer),
        }.finish()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let fields = fields(bytes)?;
        let kind = single_field(&fields, 1)?;
        let id = single_field(&fields, 2)?;
        let number = optional_field(&fields, 3)?;
        let name = optional_field(&fields, 4)?;
        let writer = optional_field(&fields, 5)?;
        let mut events = fields.iter().filter(|f| f.0 == 6).map(|f| f.1.try_into().ok());
        let no_field = number.is_none() && name.is_none() && writer.is_none();
        Some(match kind {
            [1] if no_field && events.next().is_none() => Self::Record(id.try_into().ok()?),
            [2] if events.next().is_none() => Self::EntityField {
                entity: id.try_into().ok()?, field: copy_key(number, name, true)?, writer: writer?.try_into().ok()?,
            },
            [3] if no_field => Self::DocLine { doc: id.try_into().ok()?, events: events.collect::<Option<_>>()? },
            [4] if no_field && events.next().is_none() && id.first() == Some(&4) => Self::SharePeer { peer: id.try_into().ok()? },
            _ => return None,
        })
    }
}

impl CopyRef {
    fn encode(&self) -> Vec<u8> {
        let mut w = FieldWriter::default();
        if let Some(entity) = &self.target_entity { w = w.bytes(1, entity); }
        w = self.target_field.encode(w, 2, 3);
        for source in &self.sources { w = w.bytes(4, &source.encode()); }
        w.finish()
    }

    fn decode(bytes: &[u8], set: bool, open: bool) -> Option<Self> {
        let fields = fields(bytes)?;
        let target_entity = match (set, optional_field(&fields, 1)?) {
            (true, Some(id)) => Some(id.try_into().ok()?),
            (false, None) => None,
            _ => return None,
        };
        let target_field = copy_key(optional_field(&fields, 2)?, optional_field(&fields, 3)?, open)?;
        let sources = fields.iter().filter(|f| f.0 == 4).map(|f| CopySource::decode(f.1)).collect::<Option<Vec<_>>>()?;
        if sources.is_empty() { return None }
        Some(Self { target_entity, target_field, sources })
    }
}

pub type JobId = [u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Service { Summary = 1, Tasks = 2, Transcription = 3 }

impl Service {
    fn from_code(code: u8) -> Option<Self> {
        match code { 1 => Some(Self::Summary), 2 => Some(Self::Tasks), 3 => Some(Self::Transcription), _ => None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    NotOffered, NoTranscript, NoModel(String), Interrupted,
    /// The project a request named is gone, deleted or archived: nothing was done.
    ProjectGone,
}

/// A named chat has one author or one recipient. A reply never wakes an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAgent {
    Author(Id),
    Recipient(Id),
}

/// Events that are void from the signed header alone, including after redaction.
pub fn structurally_void(kind: u16, actor: Actor) -> bool {
    actor == Actor::Ai && matches!(kind, kind::PROFILE | kind::MEMBER | kind::AGENT | kind::SAID | kind::SERVICE)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionOp {
    Open { session: SessionId, mode: SessionMode, to: Vec<Id>, town_wide: bool },
    Join { session: SessionId },
    Leave { session: SessionId, secs: Option<u32>, why: LeaveWhy },
    Decline { session: SessionId, why: DeclineWhy },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionMode { Call { video: bool }, Share { title: String } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LeaveWhy { Ended = 1, Dropped = 2, Failed = 3 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeclineWhy { Declined = 1, Busy = 2 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaidLine { pub at_ms: u32, pub dur_ms: u32, pub text: String }

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
    optional_field(fields, tag)?
}

fn optional_field<'a>(fields: &[(u8, &'a [u8])], tag: u8) -> Option<Option<&'a [u8]>> {
    let mut matches = fields.iter().filter(|f| f.0 == tag);
    let bytes = matches.next().map(|f| f.1);
    matches.next().is_none().then_some(bytes)
}

impl LeaveWhy {
    fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes { [1] => Some(Self::Ended), [2] => Some(Self::Dropped), [3] => Some(Self::Failed), _ => None }
    }
}

impl DeclineWhy {
    fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes { [1] => Some(Self::Declined), [2] => Some(Self::Busy), _ => None }
    }
}

impl SessionOp {
    fn encode(&self) -> Vec<u8> {
        let op = |tag, session: &SessionId| FieldWriter::default().bytes(1, &[tag]).bytes(2, session);
        match self {
            Self::Open { session, mode, to, town_wide } => {
                let w = op(1, session);
                let mut w = match mode {
                    SessionMode::Call { video: false } => w.bytes(3, &[1]),
                    SessionMode::Call { video: true } => w.bytes(3, &[1]).bytes(4, &[]),
                    SessionMode::Share { title } => w.bytes(3, &[2]).text(5, title),
                };
                for seat in to { w = w.bytes(6, seat); }
                if *town_wide { w = w.bytes(9, &[]); }
                w
            }
            Self::Join { session } => op(2, session),
            Self::Leave { session, secs, why } => {
                let mut w = op(3, session);
                if let Some(secs) = secs { w = w.bytes(7, &secs.to_le_bytes()); }
                w.bytes(8, &[*why as u8])
            }
            Self::Decline { session, why } => op(4, session).bytes(8, &[*why as u8]),
        }.finish()
    }

    fn decode(fields: &[(u8, &[u8])]) -> Option<Self> {
        let get = |tag| single_field(fields, tag);
        let session = get(2)?.try_into().ok()?;
        let has = |tags: std::ops::RangeInclusive<u8>| fields.iter().any(|f| tags.contains(&f.0));
        Some(match get(1)? {
            [1] if !has(7..=8) => {
                let mode = match get(3)? {
                    [1] if !has(5..=5) => SessionMode::Call { video: match optional_field(fields, 4)? {
                        None => false, Some([]) => true, _ => return None,
                    } },
                    [2] if !has(4..=4) => SessionMode::Share { title: std::str::from_utf8(get(5)?).ok()?.into() },
                    _ => return None,
                };
                let to = fields.iter().filter(|f| f.0 == 6).map(|f| f.1.try_into().ok()).collect::<Option<_>>()?;
                let town_wide = match optional_field(fields, 9)? {
                    None => false, Some([]) => true, _ => return None,
                };
                Self::Open { session, mode, to, town_wide }
            }
            [2] if !has(3..=9) => Self::Join { session },
            [3] if !has(3..=6) && !has(9..=9) => {
                let secs = match optional_field(fields, 7)? {
                    Some(bytes) => Some(u32::from_le_bytes(bytes.try_into().ok()?)),
                    None => None,
                };
                Self::Leave { session, secs, why: LeaveWhy::decode(get(8)?)? }
            }
            [4] if !has(3..=7) && !has(9..=9) => Self::Decline { session, why: DeclineWhy::decode(get(8)?)? },
            _ => return None,
        })
    }
}

impl SaidLine {
    fn encode(&self) -> Vec<u8> {
        FieldWriter::default().bytes(1, &self.at_ms.to_le_bytes())
            .bytes(2, &self.dur_ms.to_le_bytes()).text(3, &self.text).finish()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let fields = fields(bytes)?;
        let get = |tag| single_field(&fields, tag);
        Some(Self { at_ms: u32::from_le_bytes(get(1)?.try_into().ok()?),
            dur_ms: u32::from_le_bytes(get(2)?.try_into().ok()?), text: std::str::from_utf8(get(3)?).ok()?.into() })
    }
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

/// Repeated non-critical TLV tag on `kind::CHAT` v1 carrying mentioned seat ids (32 bytes each).
/// (Tag 9 is reserved for a future text blob.)
pub const TAG_MENTIONS: u8 = 8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chat {
    pub text: String,
    /// Addressed to the author's own AI (the `@`): a field set by the composer, never parsed
    /// from the text. It counts only on human events.
    pub to_ai: bool,
    /// A message this one quotes (one level: a quoted message's own quote brings nothing).
    pub quote: Option<Id>,
    /// Tasks it refers to (reference chips), by the id of the event that created each.
    pub tasks: Vec<Id>,
    /// Timeline anchor displayed above the composer when the person asked for context.
    pub above: Option<Id>,
    /// People (seat ids) this human chat line explicitly mentions: set by the composer, never
    /// parsed back from the text on receive.
    pub mentions: Vec<Id>,
}

#[derive(Clone, Copy)]
enum ContentForm {
    Agent, Member, Session, Service, Said, Town, Profile, Chat, Entity { open: bool }, Set { open: bool }, Text, Undo, Retract,
}

impl ContentForm {
    fn for_version(kind: u16, version: u8) -> Option<Self> {
        Some(match (kind, version) {
            (kind::AGENT, 1) => Self::Agent,
            (kind::MEMBER, 1) => Self::Member,
            (kind::SESSION, 1) => Self::Session,
            (kind::SERVICE, 1) => Self::Service,
            (kind::SAID, 1) => Self::Said,
            (kind::TOWN, 1) => Self::Town,
            (kind::PROFILE, 1) => Self::Profile,
            (kind::CHAT, 1) => Self::Chat,
            (kind::ENTITY, 1..=3) => Self::Entity { open: version == 3 },
            (kind::SET, 1..=2) => Self::Set { open: version == 2 },
            (kind::TEXT, 1) => Self::Text,
            (kind::UNDO, 1) => Self::Undo,
            (kind::RETRACT, 1) => Self::Retract,
            _ => return None,
        })
    }

    fn is_open(self) -> bool {
        matches!(self, Self::Entity { open: true } | Self::Set { open: true })
    }
}

// A projection of the actual signed write, never a skeleton supplied later by a redactor.
// Use the same key tags as the content. Values, labels and the salt occur only in the body.
fn content_shape(kind: u16, version: u8, bytes: &[u8]) -> Option<Vec<u8>> {
    if !ContentForm::for_version(kind, version).is_some_and(ContentForm::is_open) { return None }
    let mut shape = FieldWriter::default();
    match Content::decode(kind, version, bytes)? {
        Content::Entity { kind, project, derive, fields, .. } => {
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
            Content::Town { .. } => kind::TOWN,
            Content::Profile { .. } => kind::PROFILE,
            Content::Chat(_) | Content::AgentChat { .. } => kind::CHAT,
            Content::AgentAdded { .. } | Content::AgentRemoved { .. } | Content::AgentRenamed { .. } => kind::AGENT,
            Content::Session(_) => kind::SESSION,
            Content::Said { .. } => kind::SAID,
            Content::Provides { .. } | Content::Request { .. } | Content::Taken { .. } | Content::Done { .. }
                | Content::Failed { .. } | Content::Shared { .. } => kind::SERVICE,
            Content::Entity { .. } => kind::ENTITY,
            Content::Set { .. } => kind::SET,
            Content::Text { .. } => kind::TEXT,
            Content::Undo { .. } => kind::UNDO,
            Content::Retract { .. } => kind::RETRACT,
        }
    }

    pub fn version(&self) -> u8 {
        // A v1 reader must never assign the signing event's id to a derived task.
        let extended = |key: &Key, value: &Value| matches!(key, Key::Name(_)) || matches!(value, Value::Literal(_));
        match self {
            Self::Entity { kind, fields, copied_from, .. } if matches!(kind, Key::Name(_)) || fields.iter().any(|(k, v)| extended(k, v))
                || copied_from.iter().any(|r| matches!(r.target_field, Key::Name(_))) => 3,
            Self::Entity { derive: Some(_), .. } => 2,
            Self::Set { changes, copied_from, .. } if changes.iter().any(|c| extended(&c.field, &c.value))
                || copied_from.iter().any(|r| matches!(r.target_field, Key::Name(_))) => 2,
            _ => 1,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Content::Member(op) => op.encode(),
            Content::Session(op) => op.encode(),
            Content::Provides { services } => {
                let codes: Vec<_> = services.iter().map(|s| *s as u8).collect();
                FieldWriter::default().bytes(1, &[1]).bytes(5, &codes).finish()
            }
            Content::Request { job, service, session, project, provider } => {
                let mut w = FieldWriter::default().bytes(1, &[2]).bytes(2, session).bytes(5, &[*service as u8])
                    .bytes(6, job).bytes(7, provider);
                if let Some(project) = project { w = w.bytes(8, project); }
                w.finish()
            }
            Content::Taken { job } => FieldWriter::default().bytes(1, &[3]).bytes(6, job).finish(),
            Content::Done { job, text } => FieldWriter::default().bytes(1, &[4]).bytes(6, job).text(9, text).finish(),
            Content::Failed { job, why } => {
                let w = FieldWriter::default().bytes(1, &[5]).bytes(6, job);
                match why {
                    Failure::NotOffered => w.bytes(10, &[1]), Failure::NoTranscript => w.bytes(10, &[2]),
                    Failure::NoModel(text) => w.bytes(10, &[3]).text(9, text), Failure::Interrupted => w.bytes(10, &[4]),
                    Failure::ProjectGone => w.bytes(10, &[5]),
                }.finish()
            }
            Content::Shared { session, agent, line } => FieldWriter::default().bytes(1, &[6])
                .bytes(2, session).bytes(3, agent).bytes(4, line).finish(),
            Content::Said { session, lines } => {
                let mut w = FieldWriter::default().bytes(1, session);
                for line in lines { w = w.bytes(2, &line.encode()); }
                w.finish()
            }
            Content::Town { policy, tier } => FieldWriter::default().bytes(1, &policy.to_le_bytes()).bytes(2, &[*tier]).finish(),
            Content::Profile { name, colour } => FieldWriter::default().text(1, name).bytes(2, &colour.to_le_bytes()).finish(),
            Content::AgentAdded { name, kind } => FieldWriter::default().bytes(1, &[1]).text(2, name).text(3, kind).finish(),
            Content::AgentRemoved { agent } => FieldWriter::default().bytes(1, &[2]).bytes(4, agent).finish(),
            Content::AgentRenamed { agent, name } => FieldWriter::default().bytes(1, &[3]).bytes(4, agent).text(2, name).finish(),
            Content::Chat(chat) | Content::AgentChat { chat, .. } => {
                let mut w = FieldWriter::default().text(1, &chat.text);
                if let Self::AgentChat { agent, .. } = self {
                    w = match agent { ChatAgent::Author(id) => w.bytes(5, id), ChatAgent::Recipient(id) => w.bytes(6, id) };
                } else if chat.to_ai {
                    w = w.bytes(2, &[]);
                }
                if let Some(quote) = &chat.quote {
                    w = w.bytes(3, quote);
                }
                if let Some(above) = &chat.above { w = w.bytes(7, above); }
                for task in &chat.tasks {
                    w = w.bytes(4, task);
                }
                for seat in &chat.mentions {
                    w = w.bytes(TAG_MENTIONS, seat);
                }
                w.finish()
            }
            Content::Entity { kind, project, derive, fields, copied_from } => {
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
                for reference in copied_from { w = w.bytes(TAG_COPIED_FROM, &reference.encode()); }
                w.finish()
            }
            Content::Set { changes, label, copied_from } => {
                let mut w = FieldWriter::default();
                for change in changes {
                    w = w.bytes(1, &encode_field(&change.field, Some(&change.entity), &change.value));
                }
                if let Some(label) = label {
                    w = w.text(2, label);
                }
                for reference in copied_from { w = w.bytes(TAG_COPIED_FROM, &reference.encode()); }
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
            Content::Undo { of } | Content::Retract { of } => FieldWriter::default().bytes(1, of).finish(),
        }
    }

    // v1 tags: op (1), session (2), agent (3), opening line (4), service codes (5),
    // job (6), provider (7), project (8), text (9), failure (10). Only a request has a
    // closed field list: it can carry identifiers and one known service, nothing else.
    fn decode_service(fields: &[(u8, &[u8])]) -> Option<Self> {
        let get = |tag| single_field(fields, tag);
        let job = || get(6)?.try_into().ok();
        let text = || Some(std::str::from_utf8(get(9)?).ok()?.to_owned());
        let known_fields_fit = |tags: &[u8]| fields.iter().all(|(tag, _)| !(1..=10).contains(tag) || tags.contains(tag));
        Some(match get(1)? {
            [1] if known_fields_fit(&[1, 5]) => Self::Provides { services: get(5)?.iter().filter_map(|&code| Service::from_code(code)).collect() },
            [2] if fields.iter().all(|(tag, _)| matches!(tag, 1 | 2 | 5..=8)) => {
                let &[code] = get(5)? else { return None };
                let project = match optional_field(fields, 8)? { Some(b) => Some(b.try_into().ok()?), None => None };
                Self::Request { job: job()?, service: Service::from_code(code)?, session: get(2)?.try_into().ok()?,
                    project, provider: get(7)?.try_into().ok()? }
            }
            [3] if known_fields_fit(&[1, 6]) => Self::Taken { job: job()? },
            [4] if known_fields_fit(&[1, 6, 9]) => Self::Done { job: job()?, text: text()? },
            [5] if known_fields_fit(&[1, 6, 9, 10]) => {
                let reason = optional_field(fields, 9)?;
                let why = match (get(10)?, reason) {
                    ([1], None) => Failure::NotOffered, ([2], None) => Failure::NoTranscript,
                    ([3], Some(bytes)) => Failure::NoModel(std::str::from_utf8(bytes).ok()?.to_owned()),
                    ([4], None) => Failure::Interrupted, ([5], None) => Failure::ProjectGone, _ => return None,
                };
                Self::Failed { job: job()?, why }
            }
            [6] => Self::Shared { session: get(2)?.try_into().ok()?, agent: get(3)?.try_into().ok()?, line: get(4)?.try_into().ok()? },
            _ => return None,
        })
    }

    /// The content of a known kind and version; `None` for anything else (an unknown kind, a newer
    /// version, or malformed fields). Unknown tags inside a known kind are skipped: a field added
    /// later that an older build may ignore gets a new tag, one it must understand a new version.
    pub fn decode(kind: u16, version: u8, content: &[u8]) -> Option<Content> {
        let form = ContentForm::for_version(kind, version)?;
        let fields = fields(content)?;
        let copies = |set, open| fields.iter().filter(|f| f.0 == TAG_COPIED_FROM)
            .map(|f| CopyRef::decode(f.1, set, open)).collect::<Option<Vec<_>>>();
        let text = |tag: u8| -> Option<String> {
            match fields.iter().find(|f| f.0 == tag) {
                Some((_, v)) => String::from_utf8(v.to_vec()).ok(),
                None => Some(String::new()),
            }
        };
        let id = |value: &[u8]| -> Option<Id> { value.try_into().ok() };
        Some(match form {
            ContentForm::Agent => match single_field(&fields, 1)? {
                [1] => Self::AgentAdded {
                    name: std::str::from_utf8(single_field(&fields, 2)?).ok()?.to_owned(),
                    kind: std::str::from_utf8(single_field(&fields, 3)?).ok()?.to_owned(),
                },
                [2] => Self::AgentRemoved { agent: id(single_field(&fields, 4)?)? },
                [3] => Self::AgentRenamed {
                    agent: id(single_field(&fields, 4)?)?,
                    name: std::str::from_utf8(single_field(&fields, 2)?).ok()?.to_owned(),
                },
                _ => return None,
            },
            ContentForm::Member => Content::Member(Member::decode(&fields)?),
            ContentForm::Session => Content::Session(SessionOp::decode(&fields)?),
            ContentForm::Service => Self::decode_service(&fields)?,
            ContentForm::Said => {
                let session = single_field(&fields, 1)?.try_into().ok()?;
                let lines: Vec<_> = fields.iter().filter(|f| f.0 == 2).map(|f| SaidLine::decode(f.1)).collect::<Option<_>>()?;
                if lines.is_empty() { return None }
                Content::Said { session, lines }
            }
            ContentForm::Town => Content::Town {
                policy: u16::from_le_bytes(single_field(&fields, 1)?.try_into().ok()?),
                tier: u8::from_le_bytes(single_field(&fields, 2)?.try_into().ok()?),
            },
            ContentForm::Profile => {
                let colour = match fields.iter().find(|f| f.0 == 2) {
                    Some((_, v)) => u32::from_le_bytes((*v).try_into().ok()?),
                    None => 0,
                };
                Content::Profile { name: text(1)?, colour }
            }
            ContentForm::Chat => {
                let mut chat = Chat { text: text(1)?, ..Chat::default() };
                let mut agent = None;
                for (tag, value) in &fields {
                    match tag {
                        2 => chat.to_ai = true,
                        3 => chat.quote = Some(id(value)?),
                        4 => chat.tasks.push(id(value)?),
                        &TAG_MENTIONS => chat.mentions.push(id(value)?),
                        7 if chat.above.is_none() => chat.above = Some(id(value)?),
                        7 => return None,
                        5 | 6 => {
                            if agent.is_some() { return None }
                            agent = Some(if *tag == 5 { ChatAgent::Author(id(value)?) } else { ChatAgent::Recipient(id(value)?) });
                        }
                        _ => {}
                    }
                }
                match agent {
                    Some(agent) if !chat.to_ai => Content::AgentChat { chat, agent },
                    Some(_) => return None,
                    None => Content::Chat(chat),
                }
            }
            ContentForm::Entity { open } => {
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
                Content::Entity { kind, project, derive, fields, copied_from: copies(false, open)? }
            }
            ContentForm::Set { open } => {
                let label = match fields.iter().find(|f| f.0 == 2) {
                    Some((_, bytes)) => Some(std::str::from_utf8(bytes).ok()?.into()),
                    None => None,
                };
                let entries = fields.iter().filter(|f| f.0 == 1).map(|f| {
                    let (field, entity, value) = decode_field(f.1, open)?;
                    Some(Change { entity: entity?, field, value })
                });
                let changes = if open { entries.collect::<Option<_>>()? } else { entries.flatten().collect() };
                Content::Set { changes, label, copied_from: copies(true, open)? }
            }
            ContentForm::Text => {
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
            ContentForm::Undo => Content::Undo { of: id(fields.iter().find(|f| f.0 == 1)?.1)? },
            ContentForm::Retract => Content::Retract { of: id(fields.iter().find(|f| f.0 == 1)?.1)? },
        })
    }
}

use crate::fold::cache::Cached;
impl Cached for LeaveWhy {
    fn save(&self, out: &mut Vec<u8>) { out.push(*self as u8); }
    fn load(input: &mut &[u8]) -> Option<Self> { Self::decode(&[u8::load(input)?]) }
}
impl Cached for DeclineWhy {
    fn save(&self, out: &mut Vec<u8>) { out.push(*self as u8); }
    fn load(input: &mut &[u8]) -> Option<Self> { Self::decode(&[u8::load(input)?]) }
}
impl Cached for SessionMode {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            Self::Call { video } => out.push(u8::from(*video)),
            Self::Share { title } => { out.push(2); title.save(out); }
        }
    }
    fn load(input: &mut &[u8]) -> Option<Self> {
        match u8::load(input)? {
            0 => Some(Self::Call { video: false }), 1 => Some(Self::Call { video: true }),
            2 => Some(Self::Share { title: String::load(input)? }), _ => None,
        }
    }
}
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
impl Cached for CopySource {
    fn save(&self, out: &mut Vec<u8>) { self.encode().save(out); }
    fn load(input: &mut &[u8]) -> Option<Self> { Self::decode(&Vec::<u8>::load(input)?) }
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
    fn frozen_wire_layouts() {
        let header = Header {
            kind: kind::ENTITY, kind_version: 3, critical: false, actor: Actor::Ai, via: Via::Tool,
            author: [0x22; 65], wall_ms: 0x0102030405060708, parents: vec![[0x33; 32]], body_hash: [0x44; 32],
            shape: Some(vec![0xaa, 0xbb, 0xcc]), unknown_flags: 0, unknown_extensions: vec![],
        };
        let header_bytes = [
            &[1, 5, 0, 3, 14][..], &[0x22; 65], &[8, 7, 6, 5, 4, 3, 2, 1, 1, 0],
            &[0x33; 32], &[0x44; 32], &[3, 0, 0, 0, 0xaa, 0xbb, 0xcc],
        ].concat();
        assert_eq!(header.encode(), header_bytes, "frozen: changing this strands every copy");

        let event = Event { town: [0x55; 32], id: [0; 32], header, header_bytes: header_bytes.clone(),
            body: vec![0x66; 17], sig: vec![0x77; 64] };
        let mut frame = Vec::new();
        crate::log::encode_record(&event, &mut frame);
        assert_eq!(frame, [&[151, 0, 0, 0][..], &header_bytes, &[17, 0, 0, 0], &[0x66; 17], &[64, 0], &[0x77; 64]].concat(),
            "frozen: changing this strands every copy");

        let heads = crate::share::Message::Heads { town: [0x55; 32], heads: &[[0x88; 32]] };
        assert_eq!(heads.encode(), [&[3, 1][..], &[0x55; 32], &[0x88; 32]].concat(), "frozen: changing this strands every copy");
    }

    #[test]
    fn service_ops_round_trip_and_refuse_ambiguous_facts() {
        let job = [1; 16];
        let session = [2; 16];
        let provider = [3; 65];
        let request = Content::Request { job, session, service: Service::Tasks, project: Some([4; 32]), provider };
        let decode = |bytes: &[u8]| Content::decode(kind::SERVICE, 1, bytes);
        for fact in [
            Content::Provides { services: vec![] },
            Content::Provides { services: vec![Service::Transcription, Service::Tasks, Service::Summary] },
            request.clone(), Content::Request { job, session, service: Service::Summary, project: None, provider },
            Content::Taken { job }, Content::Done { job, text: "finished".into() },
            Content::Failed { job, why: Failure::NotOffered }, Content::Failed { job, why: Failure::Interrupted },
            Content::Failed { job, why: Failure::NoTranscript }, Content::Failed { job, why: Failure::ProjectGone }, Content::Failed { job, why: Failure::NoModel("no text model".into()) },
            Content::Shared { session, agent: [3; 32], line: [4; 32] },
        ] {
            let bytes = fact.encode();
            assert_eq!(decode(&bytes).as_ref(), Some(&fact), "round trip of {fact:?}");
            for prefix in 0..bytes.len() { let _ = decode(&bytes[..prefix]); }
            for (tag, value) in fields(&bytes).unwrap() {
                let mut repeated = bytes.clone();
                repeated.extend(FieldWriter::default().bytes(tag, value).finish());
                assert!(decode(&repeated).is_none(), "duplicate field {tag} in {fact:?}");
            }
        }
        let op = |n| FieldWriter::default().bytes(1, &[n]);
        assert_eq!(decode(&op(1).bytes(5, &[1, 254, 3]).finish()),
            Some(Content::Provides { services: vec![Service::Summary, Service::Transcription] }), "unknown offers leave known capabilities intact");
        for (tag, bad) in [(2, &session[..15]), (5, &[0xff][..]), (5, &[1, 2][..]), (6, &job[..15]),
                (7, &provider[..64]), (8, &[0; 31][..])] {
            let bytes = fields(&request.encode()).unwrap().into_iter().fold(FieldWriter::default(), |w, (t, value)|
                w.bytes(t, if tag == t { bad } else { value })).finish();
            assert!(decode(&bytes).is_none(), "invalid request field {tag}");
        }
        let mut extra = request.encode();
        extra.extend(FieldWriter::default().text(9, "instructions").finish());
        assert!(decode(&extra).is_none(), "a request cannot supply text");
        for n in [3, 4, 5] { assert!(decode(&op(n).finish()).is_none(), "op {n} requires a job"); }
        assert!(decode(&op(4).bytes(6, &job).bytes(9, &[255]).finish()).is_none(), "results must be UTF-8");
        assert!(decode(&op(5).bytes(6, &job).bytes(10, &[3]).bytes(9, &[255]).finish()).is_none(), "a model reason must be UTF-8");
        assert!(decode(&op(5).bytes(6, &job).bytes(10, &[6]).finish()).is_none(), "an unknown failure has no interpretation");
        assert!(decode(&op(5).bytes(6, &job).bytes(10, &[5]).bytes(9, b"why").finish()).is_none(), "a missing project carries no reason");
        assert!(decode(&op(6).bytes(2, &session).bytes(3, &[0; 32]).finish()).is_none(), "a share requires its opening line");
        for n in std::iter::once(0).chain(7..=255) { assert!(decode(&op(n).finish()).is_none(), "reserved service op {n} stays unknown"); }
        assert!(structurally_void(kind::SERVICE, Actor::Ai), "AI service facts are void even with an erased body");
    }

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
        let point = [4; 65];
        let id = [1; 32];
        let session = [2; 16];
        let at = CharId { event: id, index: 3 };
        for content in [
            Content::Town { policy: 2, tier: 0 },
            Content::Member(Member::Invite { subject: point, role: 0 }), Content::Member(Member::Accept { entry: id }),
            Content::Member(Member::AddDevice { seat: id, device: point }), Content::Member(Member::Remove { seat: id, device: point, last: id }),
            Content::Profile { name: "Ana".into(), colour: 42 }, Content::Chat(Chat::default()),
            Content::Chat(Chat { text: "@Ben hi".into(), mentions: vec![id, [9; 32]], ..Chat::default() }),
            Content::AgentChat { chat: Chat { quote: Some(id), tasks: vec![id], ..Chat::default() }, agent: ChatAgent::Author(id) },
            Content::AgentAdded { name: "Kit".into(), kind: "local".into() }, Content::AgentRemoved { agent: id },
            Content::AgentRenamed { agent: id, name: "Kit".into() }, Content::AgentRenamed { agent: id, name: String::new() },
            Content::Session(SessionOp::Open { session, mode: SessionMode::Call { video: true }, to: vec![id], town_wide: false }),
            Content::Session(SessionOp::Join { session }), Content::Session(SessionOp::Leave { session, secs: Some(3), why: LeaveWhy::Ended }),
            Content::Session(SessionOp::Decline { session, why: DeclineWhy::Busy }),
            Content::Said { session, lines: vec![SaidLine { at_ms: 1, dur_ms: 2, text: "Ben".into() }] },
            Content::Provides { services: vec![Service::Tasks, Service::Summary] },
            Content::Request { job: session, service: Service::Tasks, session, project: Some(id), provider: point },
            Content::Taken { job: session }, Content::Done { job: session, text: "done".into() },
            Content::Failed { job: session, why: Failure::NoModel("absent".into()) }, Content::Shared { session, agent: id, line: id },
            Content::Entity { copied_from: vec![], kind: Key::Number(1234), derive: None, project: None, fields: fields.clone() },
            Content::Entity { copied_from: vec![], kind: Key::Number(4567), derive: Some(([3; 32], CharId { event: [4; 32], index: 7 })), project: Some([2; 32]), fields },
            Content::Entity { copied_from: vec![], kind: Key::Name("Card".into()), derive: None, project: None, fields: vec![] },
            Content::Set { copied_from: vec![], changes, label: Some("tidied this board".into()) },
            Content::Set { copied_from: vec![], changes: vec![Change { entity: id, field: Key::Name("colour".into()), value: Value::Literal("red".into()) }], label: None },
            Content::Set { copied_from: vec![], changes: vec![], label: None }, Content::Undo { of: [3; 32] },
            Content::Text { doc: id, ops: vec![TextOp::Insert { anchor: BreakRef::Root, text: "Ben".into() },
                TextOp::Delete { from: at, to: at }, TextOp::Attr { at: BreakRef::Char(at), attr: 1, value: Value::Int(2) },
                TextOp::Move { block: BreakRef::Inserted(0), after: BreakRef::Root }] },
        ] {
            let bytes = content.encode();
            let decoded = Content::decode(content.kind(), content.version(), &bytes).expect("a known variant decodes");
            assert_eq!(decoded, content, "round trip for {content:?}");
            assert_eq!(decoded.encode(), bytes, "canonical content bytes for {content:?}");
        }
        for reserved in [3, 4] {
            assert_eq!(Content::decode(reserved, 1, &[]), None, "kind {reserved} stays reserved");
        }
    }

    #[test]
    fn session_and_transcript_v1_require_unambiguous_facts() {
        let session = [42; 16];
        let line = SaidLine { at_ms: 1234, dur_ms: 987, text: "hello".into() };
        let said = Content::Said { session, lines: vec![line.clone(), SaidLine { at_ms: 9, dur_ms: 0, text: "é".into() }] };
        let open = |mode| SessionOp::Open { session, mode, to: vec![[1; 32], [9; 32]], town_wide: true };
        let ops = [
            open(SessionMode::Call { video: true }), open(SessionMode::Call { video: false }),
            open(SessionMode::Share { title: "Canvas".into() }), SessionOp::Join { session },
            SessionOp::Leave { session, secs: Some(u32::MAX), why: LeaveWhy::Ended },
            SessionOp::Leave { session, secs: Some(0), why: LeaveWhy::Dropped },
            SessionOp::Leave { session, secs: None, why: LeaveWhy::Failed },
            SessionOp::Decline { session, why: DeclineWhy::Declined }, SessionOp::Decline { session, why: DeclineWhy::Busy },
        ];
        for content in ops.into_iter().map(Content::Session).chain([said]) {
            let kind = match content { Content::Said { .. } => kind::SAID, _ => kind::SESSION };
            assert_eq!((content.kind(), content.version()), (kind, 1), "stable kind and version for {content:?}");
            let mut bytes = content.encode();
            for end in 0..bytes.len() { let _ = Content::decode(kind, 1, &bytes[..end]); }
            bytes.extend(FieldWriter::default().bytes(99, &[0xff]).finish());
            assert_eq!(Content::decode(kind, 1, &bytes).as_ref(), Some(&content), "round trip with an extra tag for {content:?}");
        }

        let op = |code, id: &[u8]| FieldWriter::default().bytes(1, &[code]).bytes(2, id);
        let card_bytes = op(1, &session).bytes(3, &[1]).bytes(6, &[1; 32]).finish();
        for town_wide in [false, true] {
            let content = Content::Session(SessionOp::Open {
                session, mode: SessionMode::Call { video: false }, to: vec![[1; 32]], town_wide,
            });
            let mut expected = card_bytes.clone();
            if town_wide { expected.extend([9, 0, 0, 0, 0]); }
            assert_eq!(content.encode(), expected, "only town-wide opens append the five-byte marker after the seats");
            assert_eq!(Content::decode(kind::SESSION, 1, &expected), Some(content), "session v1 retains the explicit scope");
        }
        for (case, invalid) in [
            ("repeated op", op(2, &session).bytes(1, &[2])), ("unknown op", op(99, &session)),
            ("short session id", op(2, &session[..15])), ("unknown mode", op(1, &session).bytes(3, &[0])),
            ("nonempty scope", op(1, &session).bytes(3, &[1]).bytes(9, &[0])),
            ("repeated scope", op(1, &session).bytes(3, &[1]).bytes(9, &[]).bytes(9, &[])),
            ("scope on join", op(2, &session).bytes(9, &[])),
            ("scope on leave", op(3, &session).bytes(8, &[1]).bytes(9, &[])),
            ("scope on decline", op(4, &session).bytes(8, &[1]).bytes(9, &[])),
            ("nonempty video", op(1, &session).bytes(3, &[1]).bytes(4, &[0])),
            ("repeated video", op(1, &session).bytes(3, &[1]).bytes(4, &[]).bytes(4, &[])),
            ("invalid title", op(1, &session).bytes(3, &[2]).bytes(5, &[0xff])),
            ("short seat id", op(1, &session).bytes(3, &[1]).bytes(6, &session)),
            ("repeated seconds", op(3, &session).bytes(7, &[0; 4]).bytes(7, &[0; 4]).bytes(8, &[1])),
            ("short seconds", op(3, &session).bytes(7, &[0]).bytes(8, &[1])),
            ("unknown leave reason", op(3, &session).bytes(8, &[4])), ("unknown decline reason", op(4, &session).bytes(8, &[3])),
        ] {
            assert_eq!(Content::decode(kind::SESSION, 1, &invalid.finish()), None, "accepted {case}");
        }
        let batch = FieldWriter::default().bytes(1, &session).finish();
        assert_eq!(Content::decode(kind::SAID, 1, &batch), None, "an empty transcript batch has no statement");
        let mut repeated = line.encode();
        repeated.extend(FieldWriter::default().text(3, "ambiguous").finish());
        let bad_text = FieldWriter::default().bytes(1, &[0; 4]).bytes(2, &[0; 4]).bytes(3, &[0xff]).finish();
        for (case, bad_line) in [("repeated text", repeated), ("invalid text", bad_text), ("overlong field", vec![1, 255, 255, 255, 255])] {
            let bytes = FieldWriter::default().bytes(1, &session).bytes(2, &line.encode()).bytes(2, &bad_line).finish();
            assert_eq!(Content::decode(kind::SAID, 1, &bytes), None, "a line with {case} invalidates its whole batch");
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
        assert_eq!(Content::decode(kind::SET, 1, &bytes), Some(Content::Set { copied_from: vec![],
            changes: vec![Change { entity, field: Key::Number(2), value: Value::Int(6) }, Change { entity, field: Key::Number(4), value: Value::Null }],
            label: None,
        }), "known changes on both sides survive an unknown value");
    }

    #[test]
    fn erased_open_changes_keep_only_authenticated_shape() {
        let key = DeviceKey::generate().unwrap();
        let (town, _) = Event::found(&key, 1, 0, 0, [0; 16]).unwrap();
        let source = Value::Literal("Private{secret: 1234}".into());
        let name = Key::Name("diagram".into());
        for content in [
            Content::Entity { copied_from: vec![], kind: Key::Name("FutureCard".into()), project: None, derive: None,
                fields: vec![(name.clone(), source.clone())] },
            Content::Set { copied_from: vec![], changes: vec![Change { entity: [1; 32], field: name, value: source }],
                label: Some("private group".into()) },
        ] {
            let bytes = content.encode();
            let event = Event::create(&key, town, Draft { kind: content.kind(), kind_version: content.version(),
                ..draft(&bytes, vec![]) }, [33; 16]).unwrap();
            let read = Event::from_parts(town, event.header_bytes.clone(), event.body.clone(), event.sig.clone()).unwrap();
            assert_eq!(Content::decode(read.header.kind, read.header.kind_version, read.content().unwrap()), Some(content));
            let erased = Event::from_parts(town, event.header_bytes.clone(), vec![], event.sig.clone()).unwrap();
            assert_eq!(erased.id, event.id);
            assert!(erased.content().is_none());
            let shape = erased.header.shape.unwrap();
            assert!(shape.windows(7).any(|s| s == b"diagram"));
            assert!(!shape.windows(6).any(|s| s == b"secret"));
            assert!(!shape.windows(7).any(|s| s == b"private"));
            let mut changed = event.header_bytes;
            *changed.last_mut().unwrap() ^= 1;
            assert_eq!(Event::from_parts(town, changed, vec![], event.sig), Err(FormatError::BadSignature));
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
        let (town, genesis) = Event::found(&key, 1, 0, 0, [0; 16]).unwrap();
        let other = DeviceKey::generate().unwrap();
        let (other_town, _) = Event::found(&other, 1, 0, 0, [0; 16]).unwrap();
        assert_ne!(town, other_town, "the founder contributes to the town id");
        assert_eq!(Event::from_parts(town, genesis.header_bytes.clone(), vec![], genesis.sig.clone()),
            Err(FormatError::Malformed("town and member events are never redacted")), "a body-less genesis is refused");
        let wrong = key.sign(Domain::Event, &[other_town.as_slice(), &genesis.header_bytes].concat()).unwrap();
        assert_eq!(Event::from_parts(town, genesis.header_bytes, genesis.body, wrong), Err(FormatError::BadSignature),
            "genesis must be signed in its own town");
        let add = Content::Member(Member::AddDevice { seat: [3; 32], device: other.public() });
        let mut extended = add.encode();
        extended.extend(FieldWriter::default().bytes(80, b"future certificate").finish());
        assert_eq!(Content::decode(kind::MEMBER, 1, &extended), Some(add), "an unknown add_device tag is skippable");
        let chat = Content::Chat(Chat { text: "hello".into(), to_ai: true, quote: Some([3; 32]), above: Some([6; 32]), tasks: vec![[4; 32], [5; 32]], mentions: vec![[7; 32], [8; 32]] });
        let content = chat.encode();
        let event = Event::create(&key, town, Draft { parents: vec![[9; 32]], ..draft(&content, vec![]) }, [1; 16]).unwrap();
        assert_eq!(Content::decode(event.header.kind, event.header.kind_version, event.content().unwrap()), Some(chat), "content round-trips");
        assert_eq!(event.header.author, key.public(), "the author is the signing key");
        assert_eq!(Header::decode(&event.header_bytes).as_ref(), Ok(&event.header), "header round-trips");

        let again = Event::from_parts(town, event.header_bytes.clone(), event.body.clone(), event.sig.clone()).unwrap();
        assert_eq!(again, event, "an event read back is the same event");
        assert_eq!(Event::from_parts(other_town, event.header_bytes.clone(), event.body.clone(), event.sig.clone()),
            Err(FormatError::BadSignature), "a valid event cannot cross towns");
        let resigned = key.sign(Domain::Event, &[town.as_slice(), &event.header_bytes].concat()).unwrap();
        let other_sig = Event::from_parts(town, event.header_bytes.clone(), event.body.clone(), resigned).unwrap();
        assert_eq!(other_sig.id, event.id, "the id never covers the signature");

        let redacted = Event::from_parts(town, event.header_bytes.clone(), Vec::new(), event.sig.clone()).unwrap();
        assert!(redacted.redacted() && redacted.content().is_none() && redacted.id == event.id, "an erased body keeps the event");

        let mut shaped = event.header.clone();
        shaped.shape = Some(FieldWriter::default().text(11, "invented").finish());
        for (kind, version, closed) in [(kind::CHAT, 1, true), (kind::ENTITY, 1, true), (kind::ENTITY, 2, true),
            (kind::ENTITY, 4, false), (kind::SET, 1, true), (kind::SET, 3, false), (499, 1, false)] {
            shaped.kind = kind;
            shaped.kind_version = version;
            let bytes = shaped.encode();
            let sig = key.sign(Domain::Event, &[town.as_slice(), &bytes].concat()).unwrap();
            for body in [event.body.clone(), Vec::new()] {
                let read = Event::from_parts(town, bytes.clone(), body, sig.clone());
                if closed {
                    assert!(matches!(read, Err(FormatError::Malformed(_))), "a closed kind {kind} v{version} rejects a shape");
                } else {
                    let read = read.expect("a signed shape on an unknown version verifies");
                    assert!(read.header.has_unknown_extensions() && read.header.shape.is_none(), "an unknown shape stays opaque");
                    assert_eq!(read.header.encode(), bytes, "an unknown shape retains its flag and exact frame");
                }
            }
        }

        let mut header = event.header_bytes.clone();
        header[80] ^= 1;
        assert_eq!(Event::from_parts(town, header, event.body.clone(), event.sig.clone()), Err(FormatError::BadSignature), "a changed header");
        let mut body = event.body.clone();
        *body.last_mut().unwrap() ^= 1;
        assert_eq!(Event::from_parts(town, event.header_bytes.clone(), body, event.sig.clone()), Err(FormatError::BodyMismatch), "a changed body");
        let other = DeviceKey::generate().unwrap();
        let forged = other.sign(Domain::Event, &[town.as_slice(), &event.header_bytes].concat()).unwrap();
        assert_eq!(Event::from_parts(town, event.header_bytes.clone(), event.body.clone(), forged), Err(FormatError::BadSignature), "someone else's signature");
        let mut newer = event.header_bytes.clone();
        newer[0] = 2;
        assert_eq!(Event::from_parts(town, newer, Vec::new(), event.sig.clone()), Err(FormatError::NewerFormat(2)), "a newer format is named");
        for end in 0..event.header_bytes.len() {
            assert!(Header::decode(&event.header_bytes[..end]).is_err(), "a header cut at {end}");
        }

        assert_eq!(Content::decode(99, 1, &content), None, "an unknown kind is unknown as a whole");
        assert_eq!(Content::decode(kind::CHAT, 2, &content), None, "a newer version is unknown as a whole");
        assert_eq!(Content::decode(kind::CHAT, 1, &content[..content.len() - 1]), None, "cut fields are refused");
        let named = Content::AgentChat { chat: Chat { text: "hi".into(), ..Chat::default() }, agent: ChatAgent::Recipient([1; 32]) };
        assert_eq!((named.kind(), named.version()), (kind::CHAT, 1));
        assert_eq!(Content::decode(kind::CHAT, 1, &named.encode()), Some(named.clone()));
        let old_tags: Vec<_> = fields(&named.encode()).unwrap().into_iter().map(|(tag, _)| tag).filter(|tag| *tag <= 4).collect();
        assert_eq!(old_tags, [1], "a v1 reader sees a plain line");
        for extra in [FieldWriter::default().bytes(5, &[1; 32]), FieldWriter::default().bytes(6, &[2; 32]),
            FieldWriter::default().bytes(2, &[])] {
            assert!(Content::decode(kind::CHAT, 1, &[named.encode(), extra.finish()].concat()).is_none(), "ambiguous routing tags are refused");
        }
        assert!(Content::decode(kind::CHAT, 1, &FieldWriter::default().bytes(6, &[2; 31]).finish()).is_none());
        let plain = Chat { text: "@Ben hi".into(), ..Chat::default() };
        let mentioned = Content::Chat(Chat { mentions: vec![[1; 32], [2; 32]], ..plain.clone() });
        let bytes = mentioned.encode();
        let decoded = Content::decode(kind::CHAT, 1, &bytes).expect("a mention line decodes");
        assert_eq!(decoded, mentioned, "mentions round-trip in order");
        assert_eq!(decoded.encode(), bytes, "mentions re-encode to the same canonical bytes");
        assert_eq!(Content::Chat(plain.clone()).encode(), FieldWriter::default().text(1, "@Ben hi").finish(), "no mentions emit no tag 8 bytes");
        let stripped: Vec<u8> = fields(&bytes).unwrap().into_iter().filter(|(tag, _)| *tag != TAG_MENTIONS)
            .fold(FieldWriter::default(), |w, (tag, value)| w.bytes(tag, &value)).finish();
        assert_eq!(Content::decode(kind::CHAT, 1, &stripped), Some(Content::Chat(plain)), "a reader without tag 8 sees the same plain line");
        assert!(Content::decode(kind::CHAT, 1, &[FieldWriter::default().text(1, "x").finish(), FieldWriter::default().bytes(TAG_MENTIONS, &[1; 31]).finish()].concat()).is_none(),
            "a 31-byte mention is refused");
        let with_new_tag = FieldWriter::default().text(1, "hi").text(77, "later").finish();
        assert!(matches!(Content::decode(kind::CHAT, 1, &with_new_tag), Some(Content::Chat(c)) if c.text == "hi"), "an unknown tag is skipped");
    }
}
