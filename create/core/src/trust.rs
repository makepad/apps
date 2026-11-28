//! Per-request provenance checks and closed output types for Town AI reads.

use crate::doc::BreakRef;
use crate::entity::{field, kind, Entity};
use crate::event::{Actor, Anchor, CopySource, Id, JobId, Key, SessionId, Value};
use crate::fold::{JobState, ServiceRecord, State, TownPolicy};
use crate::hex;
use crate::key::PublicKey;
use makepad_network::digest::sha256_hash;
use makepad_strict_json::{self as json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// Enums only: no strings, no scores, no timestamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    Member { by_ai: bool },
    Cleared { by_ai: bool },
    External,
}

/// Sticky provenance class per field/record. Lattice order: Typed < Captured < External.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Typed = 0,
    Captured = 1,
    External = 2,
}

impl Class {
    pub const fn join(self, other: Self) -> Self {
        if (self as u8) >= (other as u8) { self } else { other }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Members,
    Cleared,
    Unreadable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LocalPolicy {
    #[default]
    Members,
    /// Local override only stricter than `Members` (T1 seam + unit tests).
    Cleared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    TownUnknownCritical,
    TownPolicyUnreadable,
    Hidden,
    External,
    RequiresClearance,
    MissingSource,
    ForeignTurnOpen,
    InvalidImage,
}

/// Sealed wrapper for a single value that passed `Gate`. Constructible only inside `core::trust`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForAi<T> {
    value: T,
    trust: Trust,
}

impl<T> ForAi<T> {
    pub fn trust(&self) -> Trust { self.trust }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatedSessionSummary {
    pub is_share: bool,
    pub outcome: &'static str,
    pub secs: Option<u32>,
    pub has_transcript: bool,
    pub rung: Vec<Id>,
}

impl ForAi<GatedSessionSummary> {
    pub fn summary(&self) -> &GatedSessionSummary { &self.value }
    pub fn into_summary(self) -> GatedSessionSummary { self.value }
}

impl Trust {
    fn join(self, other: Self) -> Self {
        use Trust::*;
        match (self, other) {
            (External, _) | (_, External) => External,
            (Cleared { by_ai: a }, Cleared { by_ai: b }) => Cleared { by_ai: a || b },
            (Member { by_ai: a } | Cleared { by_ai: a }, Member { by_ai: b } | Cleared { by_ai: b }) => Member { by_ai: a || b },
        }
    }
}

fn include(worst: &mut Option<Trust>, next: Option<Trust>) {
    if let Some(next) = next {
        *worst = Some(worst.map_or(next, |prior| prior.join(next)));
    }
}

fn escaped_width(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\x08' | '\x0c' => 2,
        c if c < ' ' => 6,
        c => c.len_utf8(),
    }
}

fn json_string_len(text: &str) -> usize {
    2 + text.chars().map(escaped_width).sum::<usize>()
}

fn escaped_prefix_len(text: &str, mut budget: usize) -> usize {
    let mut len = 0;
    for c in text.chars() {
        let Some(left) = budget.checked_sub(escaped_width(c)) else { break };
        budget = left;
        len += c.len_utf8();
    }
    len
}

/// Use the wire writer's exact number formatting without allocating its output.
#[derive(Default)]
struct ByteCount(usize);

impl std::fmt::Write for ByteCount {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

impl ForAi<String> {
    pub fn len(&self) -> usize { self.value.len() }
    pub fn is_empty(&self) -> bool { self.value.is_empty() }
    pub fn char_count(&self) -> usize { self.value.chars().count() }
    pub fn as_for_ai_str(&self) -> ForAi<&str> {
        ForAi { value: &self.value, trust: self.trust }
    }

    pub fn cleaned_person_label(self, is_owner: bool) -> Self {
        let mut chars = self.value.trim().chars().filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'));
        let mut name: String = chars.by_ref().take(40).collect();
        if chars.next().is_some() { name.pop(); name.push('…'); }
        let name = name.trim();
        let value = if name.is_empty() || name.eq_ignore_ascii_case("you") || name.eq_ignore_ascii_case("your ai") {
            if is_owner { "You" } else { "Peer" }
        } else { name };
        Self { value: value.to_owned(), trust: self.trust }
    }

    pub fn json_chunk(&self, offset: usize, end_limit: Option<usize>, budget: usize) -> Option<(ForAi<&str>, usize, bool)> {
        let part = self.value.get(offset..end_limit.unwrap_or(self.len()))?;
        let len = escaped_prefix_len(part, budget);
        Some((ForAi { value: &part[..len], trust: self.trust }, len, offset + len == self.len()))
    }

    pub fn prefix_by_scalars(&self, byte_offset: usize, max_scalars: usize) -> Option<(ForAi<&str>, usize, usize)> {
        let rest = self.value.get(byte_offset..)?;
        let mut bytes = 0;
        let mut count = 0;
        for c in rest.chars().take(max_scalars) {
            bytes += c.len_utf8();
            count += 1;
        }
        Some((ForAi { value: &rest[..bytes], trust: self.trust }, count, bytes))
    }

    pub fn same_trimmed_line(&self, my_spaces: usize, other: &Self, other_spaces: usize) -> bool {
        let a = self.value.trim_start_matches(' ');
        let b = other.value.trim_start_matches(' ');
        a == b && my_spaces as u128 + (self.len() - a.len()) as u128
            == other_spaces as u128 + (other.len() - b.len()) as u128
    }
}

impl<'a> ForAi<&'a Value> {
    pub fn as_i64(&self) -> Option<i64> { if let Value::Int(n) = self.value { Some(*n) } else { None } }
    pub fn as_bool(&self) -> Option<bool> { if let Value::Bool(b) = self.value { Some(*b) } else { None } }
    pub fn as_id(&self) -> Option<Id> { if let Value::Id(id) = self.value { Some(*id) } else { None } }
    pub fn as_blob(&self) -> Option<(u64, Id)> { if let Value::Blob { size, root } = self.value { Some((*size, *root)) } else { None } }
    pub fn as_key_bytes(&self) -> Option<&'a [u8]> { if let Value::Key(key) = self.value { Some(key) } else { None } }
    pub fn as_anchor(&self) -> Option<&'a Anchor> { if let Value::Anchor(a) = self.value { Some(a) } else { None } }
    pub fn as_text(&self) -> Option<ForAi<&'a str>> {
        let Value::Text(s) = self.value else { return None };
        Some(ForAi { value: s, trust: self.trust })
    }
    pub fn as_literal(&self) -> Option<ForAi<&'a str>> {
        let Value::Literal(s) = self.value else { return None };
        Some(ForAi { value: s, trust: self.trust })
    }
}

#[derive(Clone, Debug)]
pub struct AiJson {
    value: JsonValue,
    worst: Option<Trust>,
    // A closed node is immutable. Count children while assembling each container so
    // measuring a page needs neither a rendered String nor a traversal stack.
    encoded_len: usize,
}

impl AiJson {
    pub fn null() -> Self { Self { value: JsonValue::Null, worst: None, encoded_len: 4 } }
    pub fn bool(b: bool) -> Self { Self { value: JsonValue::Bool(b), worst: None, encoded_len: if b { 4 } else { 5 } } }
    pub fn int(n: i64) -> Self {
        let mut len = ByteCount::default();
        let _ = write!(len, "{n}");
        Self { value: JsonValue::Int(n), worst: None, encoded_len: len.0 }
    }
    pub fn f64(n: f64) -> Self {
        if !n.is_finite() { return Self::null() }
        let mut len = ByteCount::default();
        let _ = write!(len, "{n:?}");
        Self { value: JsonValue::F64(n), worst: None, encoded_len: len.0 }
    }
    pub fn static_str(s: &'static str) -> Self { Self { value: json::s(s), worst: None, encoded_len: json_string_len(s) } }
    pub fn id(id: &Id) -> Self { Self::hex_bytes(id) }
    pub fn revision(id: &Id) -> Self { Self { value: json::s(format!("@{}", hex(id))), worst: None, encoded_len: 3 + 2 * id.len() } }
    pub fn hex_bytes(bytes: &[u8]) -> Self { Self { value: json::s(hex(bytes)), worst: None, encoded_len: 2 + 2 * bytes.len() } }
    pub fn text(v: ForAi<String>) -> Self {
        Self { encoded_len: json_string_len(&v.value), value: json::s(v.value), worst: Some(v.trust) }
    }
    pub fn text_str(v: ForAi<&str>) -> Self {
        Self { encoded_len: json_string_len(v.value), value: json::s(v.value), worst: Some(v.trust) }
    }
    pub fn opt_text(v: Option<ForAi<String>>) -> Self { v.map_or_else(Self::null, Self::text) }
    pub fn built_str(b: AiTextBuilder) -> Self {
        Self { encoded_len: json_string_len(&b.buf), value: json::s(b.buf), worst: b.worst }
    }

    pub fn array(items: Vec<AiJson>) -> Self {
        let mut worst = None;
        let mut encoded_len = 2 + items.len().saturating_sub(1);
        let values = items.into_iter().map(|item| {
            include(&mut worst, item.worst);
            encoded_len += item.encoded_len;
            item.value
        }).collect();
        Self { value: JsonValue::Arr(values), worst, encoded_len }
    }
    pub fn object(fields: Vec<(&'static str, AiJson)>) -> Self {
        let mut worst = None;
        let mut encoded_len = 2 + fields.len().saturating_sub(1);
        let fields = fields.into_iter().map(|(key, item)| {
            include(&mut worst, item.worst);
            encoded_len += json_string_len(key) + 1 + item.encoded_len;
            (key.to_owned(), item.value)
        }).collect();
        Self { value: JsonValue::Obj(fields), worst, encoded_len }
    }
    pub fn id_object(fields: Vec<(Id, AiJson)>) -> Self {
        let mut worst = None;
        let mut encoded_len = 2 + fields.len().saturating_sub(1);
        let fields = fields.into_iter().map(|(key, item)| {
            include(&mut worst, item.worst);
            encoded_len += 2 * key.len() + 3 + item.encoded_len;
            (hex(&key), item.value)
        }).collect();
        Self { value: JsonValue::Obj(fields), worst, encoded_len }
    }
    pub fn encoded_len(&self) -> usize { self.encoded_len }
    pub fn into_text_builder(self) -> AiTextBuilder {
        let (buf, worst) = self.into_rendered_json();
        AiTextBuilder { buf, worst }
    }
    fn into_rendered_json(self) -> (String, Option<Trust>) { (self.value.to_json(), self.worst) }
}

#[derive(Default)]
pub struct AiTextBuilder {
    buf: String,
    worst: Option<Trust>,
}

impl AiTextBuilder {
    pub fn new() -> Self { Self::default() }
    pub fn push_static(&mut self, s: &'static str) { self.buf.push_str(s); }
    pub fn push_id(&mut self, id: &Id) { self.push_hex_bytes(id); }
    pub fn push_short_id(&mut self, bytes: &[u8]) { self.push_hex_bytes(&bytes[..bytes.len().min(4)]); }
    pub fn push_hex_bytes(&mut self, bytes: &[u8]) { self.buf.push_str(&hex(bytes)); }
    pub fn push_u64(&mut self, n: u64) { let _ = write!(self.buf, "{n}"); }
    pub fn push_u64_hex(&mut self, n: u64) { let _ = write!(self.buf, "{n:x}"); }
    pub fn push_i64(&mut self, n: i64) { let _ = write!(self.buf, "{n}"); }
    pub fn push_f64(&mut self, n: f64) { let _ = write!(self.buf, "{n}"); }
    pub fn push_usize(&mut self, n: usize) { let _ = write!(self.buf, "{n}"); }
    pub fn push_spaces(&mut self, n: usize) { self.buf.extend(std::iter::repeat_n(' ', n)); }
    pub fn push_for_ai(&mut self, v: ForAi<String>) { self.push_for_ai_str(v.as_for_ai_str()); }
    pub fn push_for_ai_str(&mut self, v: ForAi<&str>) {
        include(&mut self.worst, Some(v.trust));
        self.buf.push_str(v.value);
    }
    pub fn push_builder(&mut self, other: &Self) {
        include(&mut self.worst, other.worst);
        self.buf.push_str(&other.buf);
    }
    pub fn push_splash_quoted(&mut self, v: ForAi<&str>) {
        include(&mut self.worst, Some(v.trust));
        self.buf.push('"');
        for c in v.value.chars() {
            match c {
                '"' => self.buf.push_str("\\\""),
                '\\' => self.buf.push_str("\\\\"),
                '\n' => self.buf.push_str("\\n"),
                '\r' => self.buf.push_str("\\r"),
                '\t' => self.buf.push_str("\\t"),
                c if c.is_control() => { let _ = write!(self.buf, "\\u{{{:x}}}", c as u32); }
                c => self.buf.push(c),
            }
        }
        self.buf.push('"');
    }
    pub fn push_splash_enum(&mut self, v: ForAi<&str>) {
        if crate::literal::is_identifier(v.value) {
            self.buf.push('@');
            self.push_for_ai_str(v);
        } else { self.push_splash_quoted(v); }
    }
    pub fn push_splash_color(&mut self, n: u32) {
        if n <= 0xffffff { let _ = write!(self.buf, "#{n:06x}"); }
        else { let _ = write!(self.buf, "#{n:08x}"); }
    }
    pub fn push_splash_open_field(&mut self, key: ForAi<&str>, src: ForAi<&str>) {
        let newline = src.value.rsplit('\n').next().is_some_and(|line| line.contains("//"));
        self.buf.push(' ');
        self.push_for_ai_str(key);
        self.buf.push_str(": ");
        self.push_for_ai_str(src);
        if newline { self.buf.push('\n'); }
    }
    pub fn ends_with_char(&self, c: char) -> bool { self.buf.ends_with(c) }
    pub fn truncate(&mut self, len: usize) { self.buf.truncate(self.buf.floor_char_boundary(len)); }
    pub fn len(&self) -> usize { self.buf.len() }
    pub fn is_empty(&self) -> bool { self.buf.is_empty() }
    pub fn sha256(&self) -> Id { sha256_hash(self.buf.as_bytes()) }
    pub fn json_encoded_len(&self) -> usize { self.buf.chars().map(escaped_width).sum() }
    pub fn json_fragment_bounds(&self, budget: usize) -> Vec<usize> {
        let mut bounds = vec![0];
        let mut used: usize = 0;
        for (at, c) in self.buf.char_indices() {
            let width = escaped_width(c);
            if used > 0 && width > budget.saturating_sub(used) {
                bounds.push(at);
                used = 0;
            }
            used += width;
        }
        bounds.push(self.len());
        bounds
    }
    pub fn slice_to_json(&self, start: usize, end: usize) -> Option<AiJson> {
        let text = self.buf.get(start..end)?;
        Some(AiJson { value: json::s(text), worst: self.worst, encoded_len: json_string_len(text) })
    }
    /// The budget covers escaped payload bytes. The caller reserves its two quotes.
    pub fn json_chunk_to_json(&self, offset: usize, budget: usize) -> Option<(AiJson, usize, bool)> {
        let rest = self.buf.get(offset..)?;
        let len = escaped_prefix_len(rest, budget);
        Some((self.slice_to_json(offset, offset + len)?, len, len == rest.len()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiOut {
    text: String,
    trust: Trust,
}

impl AiOut {
    pub fn trust(&self) -> Trust { self.trust }
    pub fn as_str(&self) -> &str { &self.text }
    pub fn into_string(self) -> String { self.text }
}

/// Sticky class table: `(entity_kind, field) -> Class`.
pub fn entity_field_class(entity_kind: &Key, field: &Key) -> Class {
    match field {
        Key::Number(40..=46) => Class::External, // GH_*
        Key::Number(47 | 48) => Class::External, // COPIED_FROM / POLICY never exposed
        Key::Number(14 | 20 | 24..=30 | 32..=39) => Class::Captured, // blobs, label, media, session/app/edit meta
        Key::Number(1) if matches!(entity_kind, Key::Number(kind::CLIP | kind::APP)) => Class::Captured,
        Key::Number(13) if matches!(entity_kind, Key::Number(kind::APP_EDIT)) => Class::Captured,
        Key::Number(1..=13 | 15..=19 | 21..=23 | 31) => Class::Typed,
        Key::Name(_) => Class::Typed,
        _ => Class::External, // 0 and unknown numeric fields >= 49 fail closed
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum WorkKey {
    Record(Id),
    Field(Id, Id, Key),
    DocAttr(Id, BreakRef, u16, Id),
}

type Worklist = Vec<(WorkKey, Class)>;

struct Origins {
    class: Class,
    all_authors_ok: bool,
    by_ai: bool,
    has_unrecorded_peer_source: bool,
    entities: BTreeSet<Id>,
}

pub struct Gate<'a> {
    state: &'a State,
    policy: Policy,
}

impl<'a> Gate<'a> {
    pub fn new(state: &'a State, local: LocalPolicy) -> Self {
        let policy = if state.town_policy == TownPolicy::Unreadable || state.unknown_policy.is_some() || !state.unknown_critical.is_empty() {
            Policy::Unreadable
        } else {
            match local { LocalPolicy::Members => Policy::Members, LocalPolicy::Cleared => Policy::Cleared }
        };
        Self { state, policy }
    }
    /// Whether this gate checks the same folded state the caller is traversing.
    pub fn is_bound_to(&self, state: &State) -> bool { std::ptr::eq(self.state, state) }
    pub fn policy(&self) -> Policy { self.policy }
    pub fn town_readable(&self) -> Result<(), Withheld> {
        if !self.state.unknown_critical.is_empty() { return Err(Withheld::TownUnknownCritical) }
        if self.policy == Policy::Unreadable { return Err(Withheld::TownPolicyUnreadable) }
        Ok(())
    }
    fn member(&self, key: &PublicKey) -> bool {
        self.state.active_member(key) && !self.state.is_intake(key)
    }

    fn check_edit(&self, id: &Id, work: &mut Worklist, origins: &mut Origins) -> Result<(), Withheld> {
        origins.class = origins.class.join(Class::Captured);
        origins.has_unrecorded_peer_source = true;
        let mut has_seat = false;
        for (key, value) in self.state.created_entity_fields(id).ok_or(Withheld::Hidden)? {
            if *key == field::PARENT {
                let Value::Id(parent) = value else { return Err(Withheld::Hidden) };
                if !self.state.entity_is_visible(parent) { return Err(Withheld::Hidden) }
                let creator = self.state.derived_task_meta(parent).map_or(*parent, |d| d.creator);
                work.push((WorkKey::Record(creator), Class::Typed));
            } else if *key == field::EDIT_SEAT {
                has_seat = true;
                origins.all_authors_ok &= matches!(value, Value::Key(bytes) if
                    bytes.as_slice().try_into().is_ok_and(|key: &PublicKey| self.member(key)));
            } else if (*key == field::EDIT_AGENT && matches!(value, Value::Id(_)))
                || (*key == field::EDIT_AI && *value == Value::Bool(true)) {
                origins.by_ai = true;
            }
        }
        origins.all_authors_ok &= has_seat;
        Ok(())
    }

    fn enqueue_field_if_touched(&self, entity: &crate::entity::Entity, key: &Key, work: &mut Worklist) {
        let writer = entity.fields.get(key).filter(|f| f.value != Value::Null).map(|f| f.writer)
            .or_else(|| self.state.cleared_field_writer(&entity.id, key));
        if let Some(writer) = writer {
            work.push((WorkKey::Field(writer, entity.id, key.clone()), entity_field_class(&entity.kind, key)));
        }
    }

    fn entity_origins(&self, id: &Id, work: &mut Worklist, origins: &mut Origins) -> Result<(), Withheld> {
        let derived = self.state.derived_task_meta(id);
        let entity_id = derived.map_or(*id, |d| d.task);
        if !origins.entities.insert(entity_id) { return Ok(()) }
        if let Some(derived) = derived {
            self.enqueue_doc(derived.doc, work)?;
            if let BreakRef::Char(ch) = derived.at { work.push((WorkKey::Record(ch.event), Class::Typed)); }
        }
        let Some(entity) = self.state.entities.get(&entity_id) else { return Ok(()) };
        let creator = derived.map_or(entity_id, |d| d.creator);
        work.push((WorkKey::Record(creator), Class::Typed));
        if let Some(project) = entity.project {
            let creator = self.state.derived_task_meta(&project).map_or(project, |d| d.creator);
            work.push((WorkKey::Record(creator), Class::Typed));
        }
        self.enqueue_field_if_touched(entity, &field::DELETED, work);
        self.enqueue_field_if_touched(entity, &field::ARCHIVED, work);
        if entity.kind == kind::TASK {
            self.enqueue_field_if_touched(entity, &field::MERGED_INTO, work);
            if matches!(entity.value(field::MERGED_INTO), Some(Value::Id(_))) && self.state.entity_is_visible(&entity_id) {
                let mut seen = BTreeSet::from([entity_id]);
                let mut cur = entity;
                while let Some(Value::Id(next_id)) = cur.value(field::MERGED_INTO) {
                    if !seen.insert(*next_id) { break }
                    cur = self.state.entities.get(next_id).filter(|e| e.kind == kind::TASK && e.project == entity.project)
                        .ok_or(Withheld::Hidden)?;
                    let creator = self.state.derived_task_meta(next_id).map_or(*next_id, |d| d.creator);
                    work.push((WorkKey::Record(creator), Class::Typed));
                }
            }
        }
        if entity.kind == kind::APP_EDIT {
            self.check_edit(&entity_id, work, origins)?;
        }
        Ok(())
    }
    fn enqueue_doc(&self, id: Id, work: &mut Worklist) -> Result<(), Withheld> {
        if !self.state.doc_is_visible(&id) { return Err(Withheld::Hidden) }
        work.push((WorkKey::Record(id), Class::Typed));
        Ok(())
    }
    fn expand_record(&self, id: Id, work: &mut Worklist, origins: &mut Origins) -> Result<(), Withheld> {
        use crate::event::kind as record_kind;
        let meta = self.state.record_meta(&id).ok_or(Withheld::MissingSource)?;
        if meta.void || !meta.readable || self.state.message_retracted(&id) { return Err(Withheld::Hidden) }
        origins.all_authors_ok &= self.member(&meta.author);
        origins.by_ai |= meta.actor == Actor::Ai;
        match meta.kind {
            record_kind::ENTITY => self.entity_origins(&id, work, origins)?,
            record_kind::SAID => {
                origins.class = origins.class.join(Class::Captured);
                let (session, _) = self.state.session_fact_meta(&id).ok_or(Withheld::Hidden)?;
                work.push((WorkKey::Record(self.open_anchor(&session)?), Class::Captured));
            }
            record_kind::SESSION => {
                let (session, is_open) = self.state.session_fact_meta(&id).ok_or(Withheld::Hidden)?;
                let anchor = self.open_anchor(&session)?;
                if is_open {
                    if id != anchor { return Err(Withheld::Hidden) }
                } else {
                    work.push((WorkKey::Record(anchor), Class::Typed));
                }
            }
            record_kind::SERVICE => match self.state.service_record(&id).ok_or(Withheld::Hidden)? {
                ServiceRecord::Done { job } | ServiceRecord::Failed { job } => {
                    origins.class = origins.class.join(Class::Captured);
                    let job = self.state.service_job_meta(&job).ok_or(Withheld::MissingSource)?;
                    work.push((WorkKey::Record(job.request), Class::Captured));
                }
                ServiceRecord::Request { session, .. } => {
                    origins.class = origins.class.join(Class::Captured);
                    let anchor = self.open_anchor(&session)?;
                    if self.state.has_unreadable_session_fact_before(&session, id) { return Err(Withheld::Hidden) }
                    work.push((WorkKey::Record(anchor), Class::Captured));
                    work.extend(self.state.said_before(&session, Some(id)).into_iter()
                        .map(|line| (WorkKey::Record(line.event), Class::Captured)));
                }
                ServiceRecord::Other => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn expand_field(&self, writer: Id, entity: Id, key: &Key, class: Class, work: &mut Worklist, origins: &mut Origins) -> Result<(), Withheld> {
        self.entity_origins(&entity, work, origins)?;
        if let Some(prev) = self.state.field_undo_prev(&writer, &entity, key) {
            work.push((WorkKey::Field(prev, entity, key.clone()), class));
        }
        if *key == field::STATUS {
            if let Some(derived) = self.state.derived_task_meta(&entity).filter(|d| d.creator == writer) {
                self.enqueue_derived_checkbox(derived, work)?;
            }
            if let Some((doc, at, link_writer)) = self.state.tick_link_source(&writer, &entity) {
                work.push((WorkKey::DocAttr(doc, at, crate::doc::attr::TASK, link_writer), class));
            }
        }
        for source in self.state.copied_from(writer, entity, key) {
            match source {
                CopySource::Record(src) => {
                    let meta = self.state.record_meta(src).ok_or(Withheld::MissingSource)?;
                    if !self.state.record_before(&writer, src) { return Err(Withheld::Hidden) }
                    if meta.readable && !meta.void && meta.kind == crate::event::kind::ENTITY {
                        let target = self.state.derived_task_meta(src).map_or(*src, |d| d.task);
                        if !self.state.entity_is_visible(&target) { return Err(Withheld::Hidden) }
                    }
                    work.push((WorkKey::Record(*src), Class::Typed));
                    if let Some(quote) = self.state.message(src).and_then(|m| m.chat.quote) {
                        work.push((WorkKey::Record(quote), Class::Typed));
                    }
                }
                CopySource::EntityField { entity: id, field: key, writer: origin } => {
                    let src = self.state.entities.get(id).ok_or(Withheld::Hidden)?;
                    if !self.state.entity_is_visible(id) {
                        let retired_here = src.project.is_none_or(|p| self.state.entity_is_visible(&p))
                            && !src.flag(field::ARCHIVED)
                            && src.fields.get(&field::DELETED).is_some_and(|f| f.origin == writer && f.value == Value::Bool(true))
                            && src.fields.get(&field::MERGED_INTO).is_some_and(|f| f.origin == writer && f.value == Value::Id(entity));
                        if !retired_here { return Err(Withheld::Hidden) }
                    }
                    if self.state.record_meta(origin).is_none() { return Err(Withheld::MissingSource) }
                    if !self.state.verify_field_writer_at(&writer, id, key, origin) { return Err(Withheld::Hidden) }
                    work.push((WorkKey::Field(*origin, *id, key.clone()), entity_field_class(&src.kind, key)));
                }
                CopySource::DocLine { doc, events } => {
                    self.enqueue_doc(*doc, work)?;
                    for id in events {
                        self.state.record_meta(id).ok_or(Withheld::MissingSource)?;
                        if !self.state.doc_event_before(&writer, doc, id) { return Err(Withheld::Hidden) }
                        work.push((WorkKey::Record(*id), Class::Typed));
                    }
                }
                CopySource::SharePeer { peer } => {
                    let class = if self.member(peer) { Class::Captured } else { Class::External };
                    origins.class = origins.class.join(class);
                    origins.has_unrecorded_peer_source = true;
                }
            }
        }
        Ok(())
    }

    fn eval_origin(&self, mut work: Worklist) -> Result<Trust, Withheld> {
        let mut visited = BTreeSet::new();
        let mut origins = Origins { class: Class::Typed, all_authors_ok: true, by_ai: false, has_unrecorded_peer_source: false,
            entities: BTreeSet::new() };
        while let Some((key, class)) = work.pop() {
            origins.class = origins.class.join(class);
            if !visited.insert(key.clone()) { continue }
            match key {
                WorkKey::Record(id) => self.expand_record(id, &mut work, &mut origins)?,
                WorkKey::DocAttr(doc, at, attr, writer) => {
                    work.push((WorkKey::Record(writer), class));
                    if let Some(prev) = self.state.doc_attr_undo_prev(&doc, &writer, at, attr) {
                        work.push((WorkKey::DocAttr(doc, at, attr, prev), class));
                    }
                }
                WorkKey::Field(writer, entity, field) => {
                    work.push((WorkKey::Record(writer), class));
                    self.expand_field(writer, entity, &field, class, &mut work, &mut origins)?;
                }
            }
        }
        let trust = if origins.all_authors_ok && origins.class != Class::External {
            Trust::Member { by_ai: origins.by_ai }
        } else { Trust::External };
        match self.policy {
            Policy::Unreadable => Err(Withheld::TownPolicyUnreadable),
            Policy::Cleared if origins.has_unrecorded_peer_source || !matches!(trust, Trust::Cleared { .. }) => Err(Withheld::RequiresClearance),
            Policy::Members if trust == Trust::External => Err(Withheld::External),
            _ => Ok(trust),
        }
    }

    fn open_anchor(&self, session: &SessionId) -> Result<Id, Withheld> {
        self.state.session(session).and_then(|s| s.open.as_ref()).map(|o| o.anchor).ok_or(Withheld::MissingSource)
    }
    pub fn record_visible(&self, id: &Id) -> Result<ForAi<()>, Withheld> {
        self.town_readable()?;
        Ok(ForAi { value: (), trust: self.eval_origin(vec![(WorkKey::Record(*id), Class::Typed)])? })
    }
    /// Authorizes a static placeholder using only the surviving signed header.
    pub fn redacted_header_visible(&self, id: &Id) -> Result<ForAi<()>, Withheld> {
        self.town_readable()?;
        let meta = self.state.record_meta(id).ok_or(Withheld::MissingSource)?;
        if meta.void || meta.readable || self.state.message_retracted(id) { return Err(Withheld::Hidden) }
        let trust = if self.member(&meta.author) { Trust::Member { by_ai: meta.actor == Actor::Ai } } else { Trust::External };
        match self.policy {
            Policy::Unreadable => Err(Withheld::TownPolicyUnreadable),
            Policy::Cleared => Err(Withheld::RequiresClearance),
            Policy::Members if trust == Trust::External => Err(Withheld::External),
            Policy::Members => Ok(ForAi { value: (), trust }),
        }
    }
    pub fn chat_text(&self, id: &Id) -> Result<ForAi<String>, Withheld> {
        let trust = self.record_visible(id)?.trust;
        let chat = self.state.message(id).ok_or(Withheld::Hidden)?;
        Ok(ForAi { value: chat.chat.text.clone(), trust })
    }

    fn creator(&self, id: &Id) -> Result<Id, Withheld> {
        if !self.state.entities.contains_key(id) && self.state.record_meta(id).is_none() { return Err(Withheld::MissingSource) }
        if !self.state.entity_is_visible(id) { return Err(Withheld::Hidden) }
        Ok(self.state.derived_task_meta(id).map_or(*id, |d| d.creator))
    }
    pub fn entity_visible(&self, entity: &Id) -> Result<ForAi<()>, Withheld> {
        self.town_readable()?;
        self.record_visible(&self.creator(entity)?)
    }
    fn placement_id(&self, entity: &Entity, key: Key, missing: Withheld, work: &mut Worklist) -> Result<Id, Withheld> {
        let class = entity_field_class(&entity.kind, &key);
        if let Some(field) = entity.fields.get(&key).filter(|f| f.value != Value::Null) {
            let Value::Id(id) = field.value else { return Err(Withheld::Hidden) };
            work.push((WorkKey::Field(field.writer, entity.id, key), class));
            return Ok(id)
        }
        if let Some(clearer) = self.state.cleared_field_writer(&entity.id, &key) {
            work.push((WorkKey::Field(clearer, entity.id, key), class));
        } else if missing == Withheld::Hidden {
            return Err(missing)
        }
        self.eval_origin(std::mem::take(work))?;
        Err(missing)
    }
    pub fn board_item_placement(&self, item: &Id, board: &Id) -> Result<ForAi<()>, Withheld> {
        self.town_readable()?;
        let mut work = vec![(WorkKey::Record(self.creator(item)?), Class::Typed)];
        let item = self.state.entities.get(item).ok_or(Withheld::MissingSource)?;
        let mut next = self.placement_id(item, field::PARENT, Withheld::MissingSource, &mut work)?;
        let mut sections = BTreeSet::new();
        if item.kind == kind::SECTION { sections.insert(item.id); }
        let actual_board = loop {
            let ancestor = self.state.entities.get(&next).ok_or(Withheld::Hidden)?;
            if ancestor.kind == kind::BOARD { break next }
            if ancestor.kind != kind::SECTION { return Err(Withheld::Hidden) }
            if !sections.insert(next) {
                let home = self.placement_id(item, field::BOARD, Withheld::MissingSource, &mut work)?;
                if !self.state.entities.get(&home).is_some_and(|e| e.kind == kind::BOARD) { return Err(Withheld::Hidden) }
                break home
            }
            // A tombstoned section still determines placement through its parent.
            if ancestor.project.is_some_and(|p| !self.state.entity_is_visible(&p)) { return Err(Withheld::Hidden) }
            let creator = self.state.derived_task_meta(&next).map_or(next, |d| d.creator);
            work.push((WorkKey::Record(creator), Class::Typed));
            next = self.placement_id(ancestor, field::PARENT, Withheld::Hidden, &mut work)?;
        };
        work.push((WorkKey::Record(self.creator(&actual_board)?), Class::Typed));
        if actual_board != *board { return Err(Withheld::Hidden) }
        Ok(ForAi { value: (), trust: self.eval_origin(work)? })
    }
    fn field_state(&self, id: &Id, key: &Key) -> Result<ForAi<&'a crate::entity::FieldState>, Withheld> {
        self.town_readable()?;
        let creator = self.creator(id)?;
        let entity = self.state.entities.get(id).ok_or(Withheld::MissingSource)?;
        let field = entity.fields.get(key).filter(|f| f.value != Value::Null);
        let mut work = Vec::with_capacity(2);
        work.push((WorkKey::Record(creator), Class::Typed));
        if let Some(writer) = field.map(|f| f.writer).or_else(|| self.state.cleared_field_writer(id, key)) {
            work.push((WorkKey::Field(writer, *id, key.clone()), entity_field_class(&entity.kind, key)));
        }
        let trust = self.eval_origin(work)?;
        Ok(ForAi { value: field.ok_or(Withheld::MissingSource)?, trust })
    }
    pub fn entity_field_value(&self, id: &Id, key: &Key) -> Result<ForAi<&'a Value>, Withheld> {
        let field = self.field_state(id, key)?;
        Ok(ForAi { value: &field.value.value, trust: field.trust })
    }
    pub fn entity_field_writer(&self, id: &Id, key: &Key) -> Result<Id, Withheld> {
        Ok(self.field_state(id, key)?.value.writer)
    }
    pub fn created_entity_field_value(&self, id: &Id, key: &Key) -> Result<ForAi<&'a Value>, Withheld> {
        self.town_readable()?;
        let creator = self.creator(id)?;
        let entity = self.state.entities.get(id).ok_or(Withheld::MissingSource)?;
        let fields = self.state.created_entity_fields(id).ok_or(Withheld::Hidden)?;
        let (_, value) = fields.iter().find(|(k, _)| k == key).ok_or(Withheld::MissingSource)?;
        if value == &Value::Null { return Err(Withheld::MissingSource) }
        let trust = self.eval_origin(vec![(WorkKey::Record(creator), Class::Typed),
            (WorkKey::Field(creator, *id, key.clone()), entity_field_class(&entity.kind, key))])?;
        Ok(ForAi { value, trust })
    }
    pub fn created_entity_field_text(&self, id: &Id, key: &Key) -> Result<ForAi<String>, Withheld> {
        let text = self.created_entity_field_value(id, key)?.as_text().ok_or(Withheld::MissingSource)?;
        Ok(ForAi { value: text.value.to_owned(), trust: text.trust })
    }
    pub fn entity_field_text(&self, id: &Id, key: &Key) -> Result<ForAi<String>, Withheld> {
        let field = self.entity_field_value(id, key)?.as_text().ok_or(Withheld::MissingSource)?;
        Ok(ForAi { value: field.value.to_owned(), trust: field.trust })
    }
    pub fn open_key_name(&self, id: &Id, key: &Key) -> Result<ForAi<String>, Withheld> {
        let trust = self.field_state(id, key)?.trust;
        let Key::Name(name) = key else { return Err(Withheld::MissingSource) };
        Ok(ForAi { value: name.clone(), trust })
    }
    pub fn entity_kind_name(&self, id: &Id) -> Result<ForAi<String>, Withheld> {
        let trust = self.entity_visible(id)?.trust;
        let entity = self.state.entities.get(id).ok_or(Withheld::MissingSource)?;
        Ok(ForAi { value: entity.kind.to_string(), trust })
    }

    fn enqueue_linked_task(&self, task: Id, work: &mut Worklist) -> Result<(), Withheld> {
        let entity = self.state.entities.get(&task).ok_or(Withheld::MissingSource)?;
        if entity.kind != kind::TASK { return Err(Withheld::Hidden) }
        let mut cur = entity;
        let mut seen = BTreeSet::new();
        loop {
            if cur.project.is_some_and(|p| !self.state.entity_is_visible(&p)) || cur.flag(field::ARCHIVED) {
                return Err(Withheld::Hidden);
            }
            let creator = self.state.derived_task_meta(&cur.id).map_or(cur.id, |d| d.creator);
            work.push((WorkKey::Record(creator), Class::Typed));
            if self.state.entity_is_visible(&cur.id) { break }
            let merged = cur.fields.get(&field::MERGED_INTO).ok_or(Withheld::Hidden)?;
            let Value::Id(next_id) = merged.value else { return Err(Withheld::Hidden) };
            let deleted = cur.fields.get(&field::DELETED).ok_or(Withheld::Hidden)?;
            if deleted.value != Value::Bool(true) || deleted.origin != merged.origin {
                return Err(Withheld::Hidden);
            }
            work.push((WorkKey::Field(merged.writer, cur.id, field::MERGED_INTO), Class::Typed));
            if !seen.insert(cur.id) { return Err(Withheld::Hidden) }
            cur = self.state.entities.get(&next_id)
                .filter(|e| e.kind == kind::TASK && e.project == cur.project)
                .ok_or(Withheld::Hidden)?;
        }
        Ok(())
    }

    fn enqueue_linked_task_status(&self, task: Id, copied_writer: Option<Id>, work: &mut Worklist) -> Result<(), Withheld> {
        self.enqueue_linked_task(task, work)?;
        let entity = self.state.entities.get(&task).ok_or(Withheld::MissingSource)?;
        if let Some(writer) = copied_writer {
            work.push((WorkKey::Field(writer, task, field::STATUS), entity_field_class(&entity.kind, &field::STATUS)));
        } else {
            self.enqueue_field_if_touched(entity, &field::STATUS, work);
        }
        Ok(())
    }

    fn enqueue_derived_checkbox(&self, derived: crate::fold::DerivedTaskMeta, work: &mut Worklist) -> Result<(), Withheld> {
        if let Some((task_writer, task_val, _)) = self.state
            .doc_attr_state_before(&derived.creator, &derived.doc, derived.at, crate::doc::attr::TASK)
            .map_err(|()| Withheld::Hidden)?
        {
            work.push((WorkKey::DocAttr(derived.doc, derived.at, crate::doc::attr::TASK, task_writer), Class::Typed));
            if let Value::Id(src_task) = task_val {
                let sw = self.state
                    .field_writer_before(&derived.creator, &src_task, &field::STATUS)
                    .map_err(|()| Withheld::Hidden)?;
                match sw {
                    Some(sw) => self.enqueue_linked_task_status(src_task, Some(sw), work)?,
                    None => self.enqueue_linked_task(src_task, work)?,
                }
                return Ok(());
            }
        }
        if let Some((cb_writer, cb_val, cb_source)) = self.state
            .doc_attr_state_before(&derived.creator, &derived.doc, derived.at, crate::doc::attr::CHECKBOX)
            .map_err(|()| Withheld::Hidden)?
        {
            work.push((WorkKey::DocAttr(derived.doc, derived.at, crate::doc::attr::CHECKBOX, cb_writer), Class::Typed));
            if cb_val != Value::Null {
                if let Some(src) = cb_source {
                    let mut unlink_event = cb_writer;
                    while let Some(prev) = self.state.doc_attr_undo_prev(
                        &derived.doc, &unlink_event, derived.at, crate::doc::attr::CHECKBOX,
                    ) {
                        unlink_event = prev;
                    }
                    if !self.state.verify_doc_task_link_at_in_past(
                        &derived.creator, &unlink_event, &derived.doc, derived.at, &src.task, &src.link_writer,
                    ) {
                        return Err(Withheld::Hidden);
                    }
                    work.push((WorkKey::DocAttr(derived.doc, derived.at, crate::doc::attr::TASK, src.link_writer), Class::Typed));
                    match src.status {
                        Some(crate::doc::CopiedStatus::TaskStatus(sw)) => {
                            if !self.state.verify_task_status_at_in_past(&derived.creator, &unlink_event, &src.task, &sw) {
                                return Err(Withheld::Hidden);
                            }
                            self.enqueue_linked_task_status(src.task, Some(sw), work)?;
                        }
                        Some(crate::doc::CopiedStatus::SameEventTick { at: tick_at, link_writer }) => {
                            if !self.state.verify_doc_task_link_at_in_past(
                                &derived.creator, &unlink_event, &derived.doc, tick_at, &src.task, &link_writer,
                            ) {
                                return Err(Withheld::Hidden);
                            }
                            work.push((WorkKey::DocAttr(derived.doc, tick_at, crate::doc::attr::TASK, link_writer), Class::Typed));
                            self.enqueue_linked_task(src.task, work)?;
                        }
                        None => {
                            self.enqueue_linked_task(src.task, work)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn block_origins(&self, doc_id: Id, doc: &crate::doc::DocState, at: BreakRef, work: &mut Worklist) -> Result<(), Withheld> {
        let at = doc.canonical(at);
        let events = doc.block_run_events(at).ok_or(Withheld::MissingSource)?;
        if let BreakRef::Char(ch) = at {
            work.push((WorkKey::Record(ch.event), Class::Typed));
        }
        work.extend(events.into_iter().map(|id| (WorkKey::Record(id), Class::Typed)));
        work.extend(doc.block_attr_writers(at).into_iter()
            .map(|(attr, writer)| (WorkKey::DocAttr(doc_id, at, attr, writer), Class::Typed)));
        if let Some(Value::Id(task)) = doc.attribute(at, crate::doc::attr::TASK) {
            self.enqueue_linked_task_status(*task, None, work)?;
        } else if let Some(src) = doc.block_checkbox_source(at) {
            work.push((WorkKey::DocAttr(doc_id, at, crate::doc::attr::TASK, src.link_writer), Class::Typed));
            match src.status {
                Some(crate::doc::CopiedStatus::TaskStatus(status_writer)) => {
                    self.enqueue_linked_task_status(src.task, Some(status_writer), work)?;
                }
                Some(crate::doc::CopiedStatus::SameEventTick { at: tick_at, link_writer }) => {
                    work.push((WorkKey::DocAttr(doc_id, tick_at, crate::doc::attr::TASK, link_writer), Class::Typed));
                    self.enqueue_linked_task(src.task, work)?;
                }
                None => {
                    self.enqueue_linked_task(src.task, work)?;
                }
            }
        }
        Ok(())
    }
    pub fn doc_block_text(&self, doc_id: &Id, at: BreakRef, text: String) -> Result<ForAi<String>, Withheld> {
        self.town_readable()?;
        let mut work = Vec::new();
        self.enqueue_doc(*doc_id, &mut work)?;
        self.block_origins(*doc_id, self.state.docs.get(doc_id).ok_or(Withheld::Hidden)?, at, &mut work)?;
        Ok(ForAi { trust: self.eval_origin(work)?, value: text })
    }
    pub fn doc_full_text(&self, doc_id: &Id) -> Result<ForAi<String>, Withheld> {
        self.town_readable()?;
        let mut work = Vec::new();
        self.enqueue_doc(*doc_id, &mut work)?;
        let doc = self.state.docs.get(doc_id).ok_or(Withheld::Hidden)?;
        for block in doc.blocks() { self.block_origins(*doc_id, doc, block.at, &mut work)?; }
        Ok(ForAi { trust: self.eval_origin(work)?, value: doc.text() })
    }
    pub fn said_line(&self, session: &SessionId, event: &Id, text: String) -> Result<ForAi<String>, Withheld> {
        self.town_readable()?;
        let anchor = self.open_anchor(session)?;
        let trust = self.eval_origin(vec![(WorkKey::Record(anchor), Class::Captured), (WorkKey::Record(*event), Class::Captured)])?;
        if !self.state.session_fact_meta(event).is_some_and(|(s, _)| s == *session) { return Err(Withheld::Hidden) }
        Ok(ForAi { value: text, trust })
    }
    pub fn session_summary(&self, session: &SessionId) -> Result<ForAi<GatedSessionSummary>, Withheld> {
        use crate::event::{DeclineWhy, SessionMode};
        use crate::fold::Seen;
        self.town_readable()?;
        let session = self.state.session(session).ok_or(Withheld::MissingSource)?;
        let open = session.open.as_ref().ok_or(Withheld::MissingSource)?;
        let mut trust = self.record_visible(&open.anchor)?.trust;
        let opener_seat = self.state.seat_of(&open.opener);
        let order = |id: &Id| self.state.order_of(id).ok_or(Withheld::MissingSource);
        let mut last_enter = BTreeMap::from([(open.opener, order(&open.anchor)?)]);
        let mut answered = false;
        for (device, join) in &session.joins {
            let seat = self.state.seat_of(device);
            if !open.admits(opener_seat, seat) { continue }
            let Ok(proof) = self.record_visible(join) else { continue };
            trust = trust.join(proof.trust);
            answered |= seat != opener_seat;
            let entry = order(join)?;
            let latest = last_enter.entry(*device).or_insert(entry);
            *latest = (*latest).max(entry);
        }
        let mut departed = BTreeSet::new();
        let mut secs = None;
        for (device, leave, duration, _) in &session.leaves {
            let Some(entered) = last_enter.get(device) else { continue };
            let Ok(proof) = self.record_visible(leave) else { continue };
            trust = trust.join(proof.trust);
            if order(leave)? > *entered { departed.insert(*device); }
            if answered { secs = secs.max(*duration); }
        }
        let outcome = if answered { "answered" } else {
            let mut declined = false;
            let mut busy = true;
            for (device, decline, why) in &session.declines {
                if open.seen(opener_seat, self.state.seat_of(device)) == Seen::Other { continue }
                let Ok(proof) = self.record_visible(decline) else { continue };
                trust = trust.join(proof.trust);
                declined = true;
                busy &= *why == DeclineWhy::Busy;
            }
            if declined { if busy { "busy" } else { "declined" } }
            else if departed.len() == last_enter.len() { "no answer" } else { "pending" }
        };
        let mut has_transcript = false;
        for lines in session.said.chunk_by(|a, b| a.2 == b.2) {
            let (_, device, event, _) = &lines[0];
            if !last_enter.contains_key(device) { continue }
            let Ok(proof) = self.record_visible(event) else { continue };
            trust = trust.join(proof.trust);
            has_transcript = true;
        }
        Ok(ForAi { value: GatedSessionSummary { is_share: matches!(open.mode, SessionMode::Share { .. }),
            outcome, secs, has_transcript, rung: open.to.clone() }, trust })
    }
    pub fn service_done(&self, job: &JobId) -> Result<ForAi<String>, Withheld> {
        self.town_readable()?;
        let meta = self.state.service_job_meta(job).ok_or(Withheld::MissingSource)?;
        let report = meta.report_writer.ok_or(Withheld::MissingSource)?;
        let text = self.state.service_jobs(&meta.session).into_iter().find_map(|j| match j.state {
            JobState::Done(text) if j.job == *job => Some(text),
            _ => None,
        }).ok_or(Withheld::MissingSource)?;
        Ok(ForAi { value: text, trust: self.eval_origin(vec![(WorkKey::Record(report), Class::Captured)])? })
    }
    pub fn service_failure_note(&self, event: &Id, text: String) -> Result<ForAi<String>, Withheld> {
        self.town_readable()?;
        Ok(ForAi { value: text, trust: self.eval_origin(vec![(WorkKey::Record(*event), Class::Captured)])? })
    }
    pub fn agent_name(&self, agent: &Id) -> Result<ForAi<String>, Withheld> {
        let trust = self.record_visible(agent)?.trust;
        Ok(ForAi { value: self.state.agents.get(agent).ok_or(Withheld::Hidden)?.name.clone(), trust })
    }
    pub fn share_peer_proof(&self, peer: &PublicKey) -> Result<ForAi<()>, Withheld> {
        self.town_readable()?;
        if self.policy == Policy::Cleared { return Err(Withheld::RequiresClearance) }
        if !self.member(peer) { return Err(Withheld::External) }
        Ok(ForAi { value: (), trust: Trust::Member { by_ai: false } })
    }
    pub fn share_peer_text(&self, peer: &PublicKey, text: String) -> Result<ForAi<String>, Withheld> {
        Ok(ForAi { trust: self.share_peer_proof(peer)?.trust, value: text })
    }
    pub fn profile_or_fallback_name(&self, key: &PublicKey, peer_label: Option<&str>, local_me: &PublicKey,
        local_chosen_name: Option<&str>) -> Result<ForAi<String>, Withheld> {
        self.town_readable()?;
        if let Some(latest) = self.state.latest_profile_event(key) {
            if let Some(profile) = self.state.profiles.get(key).filter(|p| p.writer == latest) {
                if let Ok(proof) = self.record_visible(&latest) {
                    return Ok(ForAi { value: profile.name.clone(), trust: proof.trust })
                }
            }
        } else if let Ok(proof) = self.share_peer_proof(key) {
            let label = if key == local_me { local_chosen_name } else { peer_label };
            return Ok(ForAi { value: label.unwrap_or(if key == local_me { "You" } else { "Peer" }).into(), trust: proof.trust })
        }
        Ok(ForAi { value: format!("Member {}", hex(&key[..4])), trust: Trust::Member { by_ai: false } })
    }

    pub fn finish_json(&self, root: AiJson) -> Result<AiOut, Withheld> {
        self.town_readable()?;
        let (text, worst) = root.into_rendered_json();
        Ok(AiOut { text, trust: worst.unwrap_or(Trust::Member { by_ai: false }) })
    }
    pub fn finish_text(&self, builder: AiTextBuilder) -> Result<AiOut, Withheld> {
        self.town_readable()?;
        Ok(AiOut { text: builder.buf, trust: builder.worst.unwrap_or(Trust::Member { by_ai: false }) })
    }
    pub fn ack(&self, static_json: &'static str) -> Result<AiOut, Withheld> {
        self.town_readable()?;
        Ok(AiOut { text: static_json.into(), trust: Trust::Member { by_ai: false } })
    }
    pub fn from_text(&self, text: ForAi<String>) -> Result<AiOut, Withheld> {
        self.town_readable()?;
        Ok(AiOut { text: text.value, trust: text.trust })
    }
}

pub struct HomeGate {
    policy: LocalPolicy,
    has_foreign_turn: bool,
}

impl HomeGate {
    pub fn new(policy: LocalPolicy, has_foreign_turn: bool) -> Self { Self { policy, has_foreign_turn } }
    pub fn check_outside(&self) -> Result<(), Withheld> {
        if self.has_foreign_turn { Err(Withheld::ForeignTurnOpen) } else { Ok(()) }
    }
    pub fn local_captured(&self, text: String) -> Result<ForAi<String>, Withheld> {
        self.check_outside()?;
        if self.policy == LocalPolicy::Cleared { return Err(Withheld::RequiresClearance) }
        Ok(ForAi { value: text, trust: Trust::Member { by_ai: false } })
    }
    pub fn finish_json(&self, root: AiJson) -> Result<AiOut, Withheld> {
        self.check_outside()?;
        let (text, worst) = root.into_rendered_json();
        Ok(AiOut { text, trust: worst.unwrap_or(Trust::Member { by_ai: false }) })
    }
    pub fn ack(&self, static_json: &'static str) -> Result<AiOut, Withheld> {
        self.check_outside()?;
        Ok(AiOut { text: static_json.into(), trust: Trust::Member { by_ai: false } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocState, TextOp};
    use crate::event::{Change, Chat, Content, CopyRef, Draft, Event, Failure, Member, SaidLine, Service, SessionMode, SessionOp, Via};
    use crate::fold::fold;
    use crate::key::DeviceKey;

    struct TownLog {
        keys: [DeviceKey; 3],
        events: Vec<Event>,
    }

    impl TownLog {
        fn new() -> Self {
            let keys = std::array::from_fn(|_| DeviceKey::generate().unwrap());
            let (_, genesis) = Event::found(&keys[0], 2, 0, 0, [0; 16]).unwrap();
            Self { keys, events: vec![genesis] }
        }

        fn event(&self, who: usize, actor: Actor, content: Content, critical: bool) -> Event {
            let key = &self.keys[who];
            let mut parents = vec![self.events.last().unwrap().id];
            if let Some(previous) = self.events.iter().rev().find(|e| e.header.author == key.public()) {
                if previous.id != parents[0] { parents.push(previous.id); }
            }
            let bytes = content.encode();
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical, actor, via: Via::Ui,
                wall_ms: 0, parents, content: &bytes };
            Event::create(key, self.events[0].town, draft, [0; 16]).unwrap()
        }
        fn add(&mut self, who: usize, actor: Actor, content: Content) -> Id {
            let e = self.event(who, actor, content, false);
            let id = e.id;
            self.events.push(e);
            id
        }
        fn admit(&mut self, who: usize) -> Id {
            let seat = self.add(0, Actor::Human, Content::Member(Member::Invite { subject: self.keys[who].public(), role: 0 }));
            self.add(who, Actor::Human, Content::Member(Member::Accept { entry: seat }));
            seat
        }
        fn remove(&mut self, who: usize, seat: Id) {
            let device = self.keys[who].public();
            let last = self.events.iter().rev().find(|e| e.header.author == device).unwrap().id;
            self.add(0, Actor::Human, Content::Member(Member::Remove { seat, device, last }));
        }
    }

    fn entity(kind: u16, fields: Vec<(Key, Value)>) -> Content {
        Content::Entity { kind: Key::Number(kind), project: None, derive: None, fields, copied_from: vec![] }
    }
    fn change(entity: Id, field: Key, value: Value) -> Change { Change { entity, field, value } }
    fn set(changes: Vec<Change>) -> Content { Content::Set { changes, label: None, copied_from: vec![] } }
    fn reference(target_entity: Option<Id>, target_field: Key, source: CopySource) -> CopyRef {
        CopyRef { target_entity, target_field, sources: vec![source] }
    }
    fn copy_record(src: Id) -> Content {
        Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: None,
            fields: vec![(field::TITLE, Value::Text("Kit".into())), (field::TEXT, Value::Text("Kit".into()))],
            copied_from: [field::TITLE, field::TEXT].map(|k| reference(None, k, CopySource::Record(src))).into() }
    }
    fn members(state: &State) -> Gate<'_> { Gate::new(state, LocalPolicy::Members) }

    fn block_attr(doc: Id, at: BreakRef, attr: u16, value: Value) -> Content {
        Content::Text { doc, ops: vec![TextOp::Attr { at, attr, value }] }
    }

    fn doc_break_and_cleared_attr_origins_gate_empty_blocks_and_full_text() {
        use crate::doc::{attr, CharId};
        let mut log = TownLog::new();
        let ben = log.admit(1);
        let human = Actor::Human;
        let root = BreakRef::Root;
        let doc = log.add(0, human, entity(kind::DOC, vec![]));
        log.admit(2);
        let newline = log.add(1, human, Content::Text { doc, ops: vec![TextOp::Insert { anchor: root, text: "\n".into() }] });
        let marker = CharId { event: newline, index: 0 };
        let empty = BreakRef::Char(marker);
        let derived = crate::entity::derived_task(doc, marker);
        log.add(0, human, Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: Some((doc, marker)),
            fields: vec![], copied_from: vec![] });
        let mut blocks = Vec::new();
        for (by, initial, restorer) in [(0, Value::Text("heading1".into()), 1), (1, Value::Null, 0),
            (0, Value::Text("heading1".into()), 0)] {
            let id = log.add(0, human, entity(kind::DOC, vec![]));
            log.add(by, human, block_attr(id, root, attr::KIND, initial.clone()));
            for (who, undoer, kind) in [(if by == 0 && restorer == 0 { 1 } else { 0 }, restorer, "bullet"), (0, 0, "numbered")] {
                let change = log.add(who, human, block_attr(id, root, attr::KIND, Value::Text(kind.into())));
                log.add(undoer, human, Content::Undo { of: change });
            }
            blocks.push((id, by == 1 || restorer == 1));
        }
        // Both spellings of the root must follow a live link and the status captured by unlink.
        let task = log.add(0, human, entity(kind::TASK, vec![]));
        log.add(1, human, set(vec![change(task, field::STATUS, Value::Text("done".into()))]));
        for unlink in [false, true] {
            let id = log.add(0, human, entity(kind::DOC, vec![]));
            log.add(0, human, block_attr(id, root, attr::TASK, Value::Id(task)));
            if unlink { log.add(0, human, block_attr(id, root, attr::TASK, Value::Null)); }
            blocks.push((id, true));
        }

        let session = [1; 16];
        let other = [2; 16];
        let open = |session| Content::Session(SessionOp::Open { session, mode: SessionMode::Call { video: false },
            to: vec![], town_wide: true });
        let anchor = log.add(1, human, open(session));
        log.add(0, human, open(other));
        let losing = log.add(0, human, open(session));
        let facts = [
            SessionOp::Join { session },
            SessionOp::Leave { session, secs: None, why: crate::event::LeaveWhy::Ended },
            SessionOp::Decline { session, why: crate::event::DeclineWhy::Busy },
        ].map(|op| log.add(0, human, Content::Session(op)));
        let said = log.add(0, human, Content::Said { session, lines: vec![SaidLine { at_ms: 0, dur_ms: 1, text: "Ana".into() }] });
        let copied_said = log.add(0, human, copy_record(said));
        let copied_loser = log.add(0, human, copy_record(losing));
        let chat = log.add(0, human, Content::Chat(Chat { text: "Kit".into(), ..Chat::default() }));
        let other_doc = log.add(0, human, entity(kind::DOC, vec![]));
        let edit = |doc| block_attr(doc, root, attr::KIND, Value::Text("bullet".into()));
        let same_doc = log.add(0, human, edit(doc));
        let wrong_doc = log.add(0, human, edit(other_doc));
        let parallel = log.event(2, human, edit(doc), false);
        let record_copy = log.add(0, human, copy_record(parallel.id));
        let unknown = [0xfe; 32];
        let doc_copies = [same_doc, wrong_doc, chat, parallel.id, unknown].map(|id| {
            log.add(0, human, Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: None,
                fields: vec![(field::TITLE, Value::Text("Kit".into()))],
                copied_from: vec![reference(None, field::TITLE, CopySource::DocLine { doc, events: vec![same_doc, id] })] })
        });
        log.events.push(parallel);
        for removed in [false, true] {
            if removed { log.remove(1, ben); }
            let live = fold(&log.events);
            let restored = State::restore(&log.events, &live.checkpoint(&log.events)).unwrap();
            for state in [&live, &restored] {
                let gate = members(state);
                let allowed = Ok(Trust::Member { by_ai: false });
                let expected = if removed { Err(Withheld::External) } else { allowed };
                assert_eq!(state.docs[&doc].block_run_events(empty), Some(vec![]));
                assert_eq!(gate.doc_block_text(&doc, empty, String::new()).map(|p| p.trust()), expected,
                    "an empty block includes the signer of its line break");
                assert_eq!(gate.entity_visible(&derived).map(|p| p.trust()), expected);
                assert_eq!(gate.doc_full_text(&doc).map(|p| p.trust()), expected);
                assert_eq!(gate.doc_block_text(&doc, BreakRef::Inserted(0), String::new()), Err(Withheld::MissingSource));
                let title = |id: &Id| gate.entity_field_text(id, &field::TITLE).map(|v| v.trust());
                assert_eq!(gate.record_visible(&anchor).map(|v| v.trust()), expected);
                assert_eq!(gate.record_visible(&losing), Err(Withheld::Hidden), "a readable losing Open");
                assert_eq!(title(&copied_loser), Err(Withheld::Hidden), "a copied losing Open");
                for fact in facts {
                    assert_eq!(gate.record_visible(&fact).map(|v| v.trust()), expected, "session fact follows the opener");
                }
                assert_eq!(title(&copied_said), expected, "active speaker's copied Said follows the opener");
                assert_eq!(gate.said_line(&session, &said, "Ana".into()).map(|v| v.trust()), expected);
                if !removed {
                    assert_eq!(gate.said_line(&other, &said, "Ana".into()), Err(Withheld::Hidden), "wrong session");
                }
                assert_eq!(gate.said_line(&other, &unknown, String::new()), Err(Withheld::MissingSource));
                assert_eq!(gate.chat_text(&chat).unwrap().value, "Kit");
                assert_eq!(title(&record_copy), Err(Withheld::Hidden), "non-causal Record");
                for (copy, result, reason) in [
                    (doc_copies[0], allowed, "causal DocLine"),
                    (doc_copies[1], Err(Withheld::Hidden), "wrong-doc DocLine"),
                    (doc_copies[2], Err(Withheld::Hidden), "non-doc DocLine"),
                    (doc_copies[3], Err(Withheld::Hidden), "non-causal DocLine"),
                    (doc_copies[4], Err(Withheld::MissingSource), "unknown DocLine"),
                ] {
                    assert_eq!(title(&copy), result, "{reason}");
                }
                for &(id, by_ben) in &blocks {
                    let expected = if by_ben && removed { Err(Withheld::External) } else { allowed };
                    for at in [root, BreakRef::Char(state.docs[&id].root_id())] {
                        assert_eq!(gate.doc_block_text(&id, at, String::new()).map(|p| p.trust()), expected,
                            "both root keys include contributing attribute writers and clearers");
                    }
                    assert_eq!(gate.doc_full_text(&id).map(|p| p.trust()), expected);
                }
            }
        }
    }

    fn doc_checkbox_follows_linked_task_and_unlink_copy_provenance() {
        use crate::doc::{attr, CharId};
        let root = BreakRef::Root;
        let human = Actor::Human;
        let text = |s: &str| Value::Text(s.into());
        let mut log = TownLog::new();
        let ben = log.admit(1);
        let mut ticks = Vec::new();
        for (cross_line, clean) in [(false, false), (true, false), (false, true)] {
            let doc = log.add(0, human, entity(kind::DOC, vec![]));
            let task = log.add(0, human, entity(kind::TASK, vec![]));
            let link = |at, task| block_attr(doc, at, attr::TASK, Value::Id(task));
            log.add(0, human, link(root, task));
            let mut unlinked = root;
            let mut ops = vec![TextOp::Attr { at: root, attr: attr::CHECKBOX, value: text("done") }];
            if cross_line {
                let lines = log.add(0, human, Content::Text { doc, ops: vec![TextOp::Insert { anchor: root, text: "\n\n".into() }] });
                unlinked = BreakRef::Char(CharId { event: lines, index: 0 });
                let last = BreakRef::Char(CharId { event: lines, index: 1 });
                log.add(1, human, link(root, task));
                log.add(0, human, link(unlinked, task));
                log.add(0, human, link(last, task));
                ops.push(TextOp::Attr { at: unlinked, attr: attr::TASK, value: Value::Null });
                ops.push(TextOp::Attr { at: last, attr: attr::CHECKBOX, value: text("open") });
            } else {
                if !clean {
                    let other = log.add(0, human, entity(kind::TASK, vec![]));
                    for undoer in [1, 0] {
                        let edit = log.add(0, human, link(root, other));
                        log.add(undoer, human, Content::Undo { of: edit });
                    }
                }
                ops.push(TextOp::Attr { at: root, attr: attr::TASK, value: Value::Null });
            }
            log.add(0, human, Content::Text { doc, ops });
            if clean { log.add(1, human, set(vec![change(task, field::STATUS, text("open"))])); }
            ticks.push((doc, unlinked, task, cross_line, clean));
        }
        log.remove(1, ben);
        let live = fold(&log.events);
        let restored = State::restore(&log.events, &live.checkpoint(&log.events)).unwrap();
        for state in [&live, &restored] {
            let gate = members(state);
            let allowed = Ok(Trust::Member { by_ai: false });
            for &(doc, at, task, cross_line, clean) in &ticks {
                assert_eq!(state.docs[&doc].attribute(at, attr::CHECKBOX), Some(&text("done")));
                assert_eq!(gate.doc_block_text(&doc, at, String::new()).map(|v| v.trust()),
                    if clean { allowed } else { Err(Withheld::External) }, "the unlink follows the link used at the tick");
                assert_eq!(gate.entity_field_value(&task, &field::STATUS).map(|v| v.trust()),
                    if cross_line { allowed } else { Err(Withheld::External) }, "task status follows its own last writer");
            }
        }
        // Read the value from the author's actual projection, before delivering the parallel edit.
        enum Register { Checkbox, Link, Status }
        for register in [Register::Checkbox, Register::Link, Register::Status] {
            let mut log = TownLog::new();
            let ben = log.admit(1);
            log.admit(2);
            let doc = log.add(0, human, entity(kind::DOC, vec![]));
            let [done, open] = ["done", "open"].map(|s| log.add(0, human, entity(kind::TASK, vec![(field::STATUS, text(s))])));
            if matches!(register, Register::Status) { log.add(0, human, block_attr(doc, root, attr::TASK, Value::Id(done))); }
            let write = |done_value| match register {
                Register::Link => block_attr(doc, root, attr::TASK, Value::Id(if done_value { done } else { open })),
                Register::Checkbox => block_attr(doc, root, attr::CHECKBOX, text(if done_value { "done" } else { "open" })),
                Register::Status => set(vec![change(done, field::STATUS, text(if done_value { "done" } else { "open" }))]),
            };
            log.add(0, human, write(true));
            let first = log.add(0, human, write(false));
            log.add(1, human, Content::Undo { of: first });
            let second = log.add(0, human, write(false));
            let preempt = log.event(2, human, write(false), false);
            log.add(0, human, entity(kind::TASK, vec![]));
            log.add(0, human, Content::Undo { of: second });
            let later = log.event(2, human, write(false), false);
            log.add(0, human, entity(kind::TASK, vec![]));
            let input = fold(&log.events);
            let marker = input.docs[&doc].root_id();
            assert_eq!(input.docs[&doc].attribute(root, attr::CHECKBOX), Some(&text("done")));
            let task = crate::entity::derived_task(doc, marker);
            let creator = log.add(0, human, Content::Entity { kind: Key::Number(kind::TASK), project: None,
                derive: Some((doc, marker)), fields: vec![(field::STATUS, text("done"))], copied_from: vec![] });
            for removed in [false, true] {
                if removed { log.remove(1, ben); }
                for (parallel, preempted) in [(&preempt, true), (&later, false)] {
                    let mut events = log.events.clone();
                    events.push(parallel.clone());
                    let live = fold(&events);
                    let restored = State::restore(&events, &live.checkpoint(&events)).unwrap();
                    for state in [&live, &restored] {
                        assert!(!state.record_before(&creator, &parallel.id));
                        let expected = if preempted { Err(Withheld::Hidden) } else if removed { Err(Withheld::External) }
                            else { Ok(Trust::Member { by_ai: false }) };
                        assert_eq!(members(state).entity_field_value(&task, &field::STATUS).map(|p| p.trust()), expected,
                            "derived STATUS preserves the checkbox, selecting link and status undo histories");
                    }
                }
            }
        }
    }

    fn copied_field_redos_follow_the_copiers_history() {
        let mut certified_preempted = Vec::new();
        for undo_again in [false, true] {
            let mut log = TownLog::new();
            let ben = log.admit(1);
            log.admit(2);
            let human = Actor::Human;
            let src = log.add(0, human, entity(kind::TASK, vec![(field::TITLE, Value::Text("Ana".into()))]));
            let title = |text: &str| set(vec![change(src, field::TITLE, Value::Text(text.into()))]);
            let direct = log.add(0, human, title("Lu"));
            let concurrent = |log: &TownLog| log.event(2, human, title("Kit"), false);
            let before_undo = concurrent(&log);
            log.add(0, human, entity(kind::TASK, vec![]));
            let undo = log.add(1, human, Content::Undo { of: direct });
            let before_redo = concurrent(&log);
            log.add(0, human, entity(kind::TASK, vec![]));
            let redo = log.add(0, human, Content::Undo { of: undo });
            let writer = if undo_again { log.add(0, human, Content::Undo { of: redo }) } else { redo };
            let after = concurrent(&log);
            log.add(0, human, entity(kind::TASK, vec![]));
            let copy = log.add(0, human, Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: None,
                fields: vec![(field::TITLE, Value::Text(if undo_again { "Ana" } else { "Lu" }.into()))],
                copied_from: vec![reference(None, field::TITLE, CopySource::EntityField { entity: src, field: field::TITLE, writer })] });

            for removed in [false, true] {
                if removed { log.remove(1, ben); }
                for (edit, superseded) in [(&after, None), (&before_undo, Some(direct)), (&before_redo, Some(undo))] {
                    let mut events = log.events.clone();
                    events.push(edit.clone());
                    let live = fold(&events);
                    let restored = State::restore(&events, &live.checkpoint(&events)).unwrap();
                    for state in [&live, &restored] {
                        assert!(!state.record_before(&copy, &edit.id));
                        let verifies = |id: &Id| state.verify_field_writer_at(&copy, &src, &field::TITLE, id);
                        let trust = members(state).entity_field_text(&copy, &field::TITLE).map(|p| p.trust());
                        if let Some(old) = superseded {
                            assert_eq!(state.field_writer_before(&copy, &src, &field::TITLE), Err(()));
                            assert_eq!(trust, Err(Withheld::Hidden), "the copy's redo chain was preempted");
                            assert!(!verifies(&writer));
                            certified_preempted.push(verifies(&old));
                        } else {
                            assert_eq!(state.field_writer_before(&copy, &src, &field::TITLE), Ok(Some(writer)));
                            assert!(verifies(&writer) && !verifies(&undo) && !verifies(&direct),
                                "a later parallel write preserves the exact redo writer");
                            let expected = if removed && undo_again { Err(Withheld::External) } else { Ok(Trust::Member { by_ai: false }) };
                            assert_eq!(trust, expected);
                        }
                    }
                }
            }
        }
        assert_eq!(certified_preempted, [false; 16], "a preempted redo cannot certify its earlier writer");
    }

    fn proposed_and_text_fields_keep_undo_chain_provenance() {
        copied_field_redos_follow_the_copiers_history();
        let mut log = TownLog::new();
        let human = Actor::Human;
        let ben_seat = log.admit(1);
        log.admit(2);
        let genesis = log.events[0].id;
        let resurrected = log.add(0, human, entity(kind::TASK, vec![(field::TITLE, Value::Text("Lu".into()))]));
        let hide = log.add(0, human, Content::Undo { of: resurrected });
        log.add(0, human, Content::Undo { of: hide });
        let issue = log.add(0, human, entity(kind::ISSUE, vec![]));
        let issue_writer = log.add(0, human, set(vec![change(issue, field::TEXT, Value::Text("Kit".into()))]));
        log.add(1, human, Content::Undo { of: issue_writer });
        let permitted = log.add(0, human, Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: None,
            fields: vec![(field::TITLE, Value::Text("Lu".into())), (field::TEXT, Value::Text("Kit".into()))],
            copied_from: [(resurrected, field::TITLE, resurrected), (issue, field::TEXT, issue_writer)]
                .map(|(entity, field, writer)| reference(None, field.clone(), CopySource::EntityField { entity, field, writer })).into() });
        let subjects = [(1, human, 0), (0, Actor::Ai, 0), (0, human, 1)].map(|(restorer, actor, overwriter)| {
            let task = log.add(0, human, entity(kind::TASK, vec![
                (field::TITLE, Value::Text("v1".into())), (field::TEXT, Value::Text("v1".into())),
                (field::PROPOSED, Value::Bool(false))]));
            for (who, by, restorer, actor, text) in [(overwriter, human, restorer, actor, "v2"), (0, human, 0, human, "v3")] {
                let overwrite = log.add(who, by, set(vec![change(task, field::TITLE, Value::Text(text.into())),
                    change(task, field::TEXT, Value::Text(text.into())), change(task, field::PROPOSED, Value::Bool(true))]));
                log.add(restorer, actor, Content::Undo { of: overwrite });
            }
            (task, restorer == 1, actor == Actor::Ai)
        });
        let src = log.add(0, human, entity(kind::TASK, vec![]));
        let dst = log.add(0, human, entity(kind::TASK, vec![]));
        let edit = |title: &str, proposed| set(vec![
            change(src, field::TITLE, Value::Text(title.into())),
            change(src, field::TEXT, Value::Text(title.into())),
            change(src, field::PROPOSED, Value::Bool(proposed)),
            change(dst, field::TEXT, Value::Text(title.into())),
        ]);
        let original = log.add(0, human, edit("v1", false));
        let second = log.add(0, human, edit("v2", true));
        log.add(1, human, Content::Undo { of: second });
        let third = log.add(0, human, edit("v3", true));
        // Kit edits on a parallel branch. The unrelated intervening event gives a
        // strict canonical order independent of the three generated public keys.
        let parallel_edit = || set(vec![change(src, field::TITLE, Value::Text("Kit".into()))]);
        let before_undo = log.event(2, human, parallel_edit(), false);
        log.add(0, human, entity(kind::TASK, vec![]));
        let restored = log.add(0, human, Content::Undo { of: third });
        let after_undo = log.event(2, human, parallel_edit(), false);
        log.add(0, human, entity(kind::TASK, vec![]));

        let fields = [
            (field::TITLE, src, field::TITLE, restored),
            (field::TEXT, dst, field::TEXT, restored),
            (Key::Name("stale".into()), src, field::TITLE, original),
            (Key::Name("superseded".into()), src, field::TITLE, second),
            (Key::Name("undone".into()), src, field::TITLE, third),
            (Key::Name("unrelated".into()), src, field::TITLE, genesis),
            (Key::Name("unknown".into()), src, field::TITLE, [0xfe; 32]),
        ];
        let mut changes = vec![change(src, field::DELETED, Value::Bool(true)), change(src, field::MERGED_INTO, Value::Id(dst))];
        let mut copied_from = Vec::new();
        for (target, entity, field, writer) in &fields {
            let value = if matches!(target, Key::Name(_)) { Value::Literal("\"v1\"".into()) } else { Value::Text("v1".into()) };
            changes.push(change(dst, target.clone(), value));
            copied_from.push(reference(Some(dst), target.clone(), CopySource::EntityField {
                entity: *entity, field: field.clone(), writer: *writer }));
        }
        let copy_input = fold(&log.events);
        assert_eq!(copy_input.entities[&src].fields[&field::TITLE].writer, restored,
            "Ana copied the value restored by her second undo");
        let copy = log.add(0, human, Content::Set { changes, label: None, copied_from });
        log.add(0, human, set(vec![change(src, field::TITLE, Value::Text("Lu".into()))]));
        let mut superseded_writer_verified = Vec::new();
        for removed in [false, true] {
            if removed { log.remove(1, ben_seat); }
            for parallel in [None, Some(&after_undo), Some(&before_undo)] {
                let mut events = log.events.clone();
                if let Some(edit) = parallel { events.push(edit.clone()); }
                let live = fold(&events);
                let checkpoint = State::restore(&events, &live.checkpoint(&events)).unwrap();
                for state in [&live, &checkpoint] {
                    for (key, value) in [(field::TITLE, "Lu"), (field::TEXT, "Kit")] {
                        let copy = members(state).entity_field_text(&permitted, &key).unwrap();
                        assert_eq!((copy.value.as_str(), copy.trust()), (value, Trust::Member { by_ai: false }),
                            "creation undo preserves TITLE and Ben's denied undo preserves ISSUE.TEXT");
                    }
                    let read = |key: &Key| members(state).entity_field_value(&dst, key).map(|p| p.trust());
                    for label in ["stale", "superseded", "undone", "unrelated"] {
                        assert_eq!(read(&Key::Name(label.into())), Err(Withheld::Hidden),
                            "a copied field must name its effective writer ({label})");
                    }
                    assert_eq!(read(&Key::Name("unknown".into())), Err(Withheld::MissingSource));
                    let expected = if removed { Err(Withheld::External) } else { Ok(Trust::Member { by_ai: false }) };
                    assert_eq!(read(&field::TEXT), expected, "a self-copy traces the previous TEXT writer");
                    let raced = parallel.is_some_and(|edit| edit.id == before_undo.id);
                    let writer_before = if raced { Err(()) } else { Ok(Some(restored)) };
                    assert_eq!(state.field_writer_before(&copy, &src, &field::TITLE), writer_before);
                    assert_eq!(read(&field::TITLE), if raced { Err(Withheld::Hidden) } else { expected },
                        "only the edit that preempted the undo invalidates its signed writer");
                    if raced {
                        assert!(state.record_before(&copy, &restored));
                        assert!(!state.record_before(&copy, &before_undo.id));
                        superseded_writer_verified.push(state.verify_field_writer_at(&copy, &src, &field::TITLE, &third));
                    }
                    for (task, by_ben, by_ai) in subjects {
                        let expected = if removed && by_ben { Err(Withheld::External) } else { Ok(Trust::Member { by_ai }) };
                        assert_eq!(members(state).entity_field_value(&task, &field::PROPOSED).map(|p| p.trust()), expected);
                        for key in [field::TITLE, field::TEXT] {
                            assert_eq!(members(state).entity_field_text(&task, &key).map(|p| p.trust()), expected);
                        }
                    }
                }
            }
        }
        assert_eq!(superseded_writer_verified, [false; 4],
            "a concurrent edit outside the copy's past cannot certify the writer Ana already undid");
    }

    fn board_item_placement_gates_deleted_sections_and_parent_cycles() {
        fn child(log: &mut TownLog, kind: u16, parent: Id) -> Id {
            log.add(0, Actor::Human, entity(kind, vec![(field::PARENT, Value::Id(parent))]))
        }
        fn restore_twice(log: &mut TownLog, entity: Id, key: Key, value: Value) {
            for who in [1, 0] {
                let edit = log.add(0, Actor::Human, set(vec![change(entity, key.clone(), value.clone())]));
                log.add(who, Actor::Human, Content::Undo { of: edit });
            }
        }
        let mut log = TownLog::new();
        let human = Actor::Human;
        let ben = log.admit(1);
        let [board, other] = [(); 2].map(|_| log.add(0, human, entity(kind::BOARD, vec![])));
        let item = child(&mut log, kind::STICKY, board);
        restore_twice(&mut log, item, field::PARENT, Value::Id(other));
        let recovered = child(&mut log, kind::STICKY, board);
        let move_item = log.add(1, human, set(vec![change(recovered, field::PARENT, Value::Id(other))]));
        log.add(0, human, Content::Undo { of: move_item });
        let mut placements = vec![(item, board, true), (recovered, board, false)];
        for (flag, deleter, parent_undos, flag_undos) in [
            (field::DELETED, 0, false, false), (field::DELETED, 0, true, false),
            (field::DELETED, 1, false, false), (field::ARCHIVED, 1, false, false),
            (field::DELETED, 0, false, true),
        ] {
            let section = child(&mut log, kind::SECTION, board);
            let inner = child(&mut log, kind::SECTION, section);
            let item = child(&mut log, kind::STICKY, inner);
            log.add(0, human, set(vec![change(section, field::PARENT, Value::Id(other))]));
            if parent_undos { restore_twice(&mut log, section, field::PARENT, Value::Id(board)); }
            log.add(deleter, human, set(vec![change(section, flag.clone(), Value::Bool(true))]));
            if flag_undos { restore_twice(&mut log, section, flag, Value::Bool(false)); }
            placements.push((item, other, parent_undos || flag_undos || deleter == 1));
        }
        let mut cleared_home = None;
        for (edge_undos, home_undos) in [(false, false), (true, false), (false, true)] {
            let a = child(&mut log, kind::SECTION, board);
            let b = child(&mut log, kind::SECTION, a);
            log.add(0, human, set(vec![change(a, field::PARENT, Value::Id(b))]));
            let item = log.add(0, human, entity(kind::STICKY, vec![(field::PARENT, Value::Id(a)), (field::BOARD, Value::Id(board))]));
            if edge_undos { restore_twice(&mut log, b, field::PARENT, Value::Id(other)); }
            if home_undos { restore_twice(&mut log, item, field::BOARD, Value::Id(other)); }
            if !edge_undos && !home_undos {
                let orphan = child(&mut log, kind::STICKY, a);
                log.add(1, human, set(vec![change(orphan, field::BOARD, Value::Null)]));
                cleared_home = Some(orphan);
            }
            placements.push((item, board, edge_undos || home_undos));
        }
        let no_parent = child(&mut log, kind::STICKY, board);
        let section = child(&mut log, kind::SECTION, board);
        let no_root = child(&mut log, kind::STICKY, section);
        log.add(1, human, set(vec![change(no_parent, field::PARENT, Value::Null), change(section, field::PARENT, Value::Null)]));
        let project = log.add(0, human, entity(kind::PROJECT, vec![]));
        let section = log.add(0, human, Content::Entity { kind: Key::Number(kind::SECTION), project: Some(project), derive: None,
            fields: vec![(field::PARENT, Value::Id(board))], copied_from: vec![] });
        let hidden_project = child(&mut log, kind::STICKY, section);
        log.add(0, human, set(vec![change(project, field::ARCHIVED, Value::Bool(true)), change(section, field::DELETED, Value::Bool(true))]));
        for removed in [false, true] {
            if removed { log.remove(1, ben); }
            let live = fold(&log.events);
            let restored = State::restore(&log.events, &live.checkpoint(&log.events)).unwrap();
            for state in [&live, &restored] {
                let gate = members(state);
                for &(item, home, uses_ben) in &placements {
                    let expected = if removed && uses_ben { Err(Withheld::External) } else { Ok(Trust::Member { by_ai: false }) };
                    assert_eq!(gate.board_item_placement(&item, &home).map(|v| v.trust()), expected);
                    let wrong = if home == board { other } else { board };
                    assert_eq!(gate.board_item_placement(&item, &wrong), Err(Withheld::Hidden));
                }
                for (id, active) in [(no_parent, Withheld::MissingSource), (no_root, Withheld::Hidden),
                    (cleared_home.unwrap(), Withheld::MissingSource)] {
                    assert_eq!(gate.board_item_placement(&id, &board), Err(if removed { Withheld::External } else { active }));
                }
                assert_eq!(gate.board_item_placement(&hidden_project, &board), Err(Withheld::Hidden));
            }
        }
    }

    fn session_summary_ignores_withheld_joins_leaves_declines_and_said() {
        use crate::event::{DeclineWhy, LeaveWhy};
        let mut log = TownLog::new();
        let ben = log.admit(1);
        let human = Actor::Human;
        let [busy, unrung, wide, ben_only, share] = [1, 2, 3, 4, 5].map(|n| [n; 16]);
        for session in [busy, unrung, wide, ben_only, share] {
            let mode = if session == share { SessionMode::Share { title: "Mira".into() } } else { SessionMode::Call { video: false } };
            log.add(usize::from(session == share), human, Content::Session(SessionOp::Open { session, mode, to: vec![ben], town_wide: true }));
        }
        log.admit(2);
        let decline = |session| Content::Session(SessionOp::Decline { session, why: DeclineWhy::Busy });
        let leave = |session, secs| Content::Session(SessionOp::Leave { session, secs: Some(secs), why: LeaveWhy::Ended });
        let say = |session| Content::Said { session, lines: vec![SaidLine { at_ms: 0, dur_ms: 1, text: "Lu".into() }] };
        log.add(1, human, decline(busy));
        log.add(2, Actor::Ai, decline(unrung));
        log.add(2, human, leave(unrung, 99));
        log.add(2, human, say(unrung));
        log.add(0, human, leave(unrung, 5));
        for session in [wide, ben_only] {
            log.add(1, human, Content::Session(SessionOp::Join { session }));
            log.add(1, human, leave(session, 999));
            log.add(1, human, say(session));
        }
        log.add(2, Actor::Ai, Content::Session(SessionOp::Join { session: wide }));
        log.add(2, human, leave(wide, 17));
        log.add(2, human, say(wide));
        log.add(1, human, leave(share, 7));
        log.add(1, human, Content::Session(SessionOp::Join { session: share }));
        for removed in [false, true] {
            if removed { log.remove(1, ben); }
            let live = fold(&log.events);
            let restored = State::restore(&log.events, &live.checkpoint(&log.events)).unwrap();
            for state in [&live, &restored] {
                let gate = members(state);
                for (id, outcome, secs, has_transcript, by_ai) in [
                    (busy, if removed { "pending" } else { "busy" }, None, false, false),
                    (unrung, "no answer", None, false, false),
                    (wide, "answered", Some(if removed { 17 } else { 999 }), true, true),
                    (ben_only, if removed { "pending" } else { "answered" }, (!removed).then_some(999), !removed, false),
                ] {
                    let result = gate.session_summary(&id).unwrap();
                    let expected = GatedSessionSummary { is_share: false, outcome, secs, has_transcript, rung: vec![ben] };
                    assert_eq!(result.summary(), &expected);
                    assert_eq!(result.trust(), Trust::Member { by_ai });
                    assert_eq!(result.into_summary(), expected);
                }
                let expected = GatedSessionSummary { is_share: true, outcome: "pending", secs: None, has_transcript: false, rung: vec![ben] };
                assert_eq!(gate.session_summary(&share).map(ForAi::into_summary),
                    if removed { Err(Withheld::External) } else { Ok(expected) }, "the winning opener and its latest entry count");
                assert_eq!(gate.session_summary(&[0xfe; 16]), Err(Withheld::MissingSource));
            }
        }
    }

    fn cleared_entity_field_withholds_instead_of_missing_source() {
        let mut log = TownLog::new();
        let ben = log.admit(1);
        let human = Actor::Human;
        let mut cleared = Vec::new();
        for (key, value, overwrites, redo) in [
            (field::STATUS, Value::Text("done".into()), 0, false),
            (field::STATUS, Value::Text("done".into()), 1, false),
            (field::STATUS, Value::Text("done".into()), 2, false),
            (field::STATUS, Value::Text("done".into()), 0, true),
            (field::PROPOSED, Value::Bool(true), 2, false),
        ] {
            let task = log.add(0, human, entity(kind::TASK, vec![(key.clone(), value.clone())]));
            let write = |value| set(vec![change(task, key.clone(), value)]);
            let clear = log.add(1, human, write(Value::Null));
            for _ in 0..overwrites {
                let overwrite = log.add(0, human, write(value.clone()));
                log.add(0, human, Content::Undo { of: overwrite });
            }
            if redo {
                let undo = log.add(0, human, Content::Undo { of: clear });
                log.add(0, human, Content::Undo { of: undo });
            }
            cleared.push((task, key));
        }
        let task = log.add(0, human, entity(kind::TASK, vec![]));
        for undoer in [1, 0] {
            let edit = log.add(0, human, set(vec![change(task, field::TEXT, Value::Text("Ana".into()))]));
            log.add(undoer, human, Content::Undo { of: edit });
        }
        cleared.push((task, field::TEXT));
        let untouched = log.add(1, human, entity(kind::TASK, vec![]));
        let own_clear = log.add(0, human, entity(kind::TASK, vec![(field::TITLE, Value::Text("Lu".into()))]));
        log.add(0, human, set(vec![change(own_clear, field::TITLE, Value::Null)]));
        for removed in [false, true] {
            if removed { log.remove(1, ben); }
            let live = fold(&log.events);
            let restored = State::restore(&log.events, &live.checkpoint(&log.events)).unwrap();
            for state in [&live, &restored] {
                let gate = members(state);
                let expected = Err(if removed { Withheld::External } else { Withheld::MissingSource });
                for (id, key) in &cleared {
                    assert_eq!(gate.entity_field_value(id, key), expected, "cleared field traces its clear and undo chain");
                }
                assert_eq!(gate.entity_field_value(&untouched, &field::TITLE), expected, "absent field traces its creator");
                for key in [field::TEXT, field::TITLE] {
                    assert_eq!(gate.entity_field_value(&own_clear, &key), Err(Withheld::MissingSource), "active clear or untouched");
                }
            }
        }
    }

    fn entity_resurrection_project_and_merge_cycle_provenance_withheld() {
        let mut log = TownLog::new();
        let ben = log.admit(1);
        let human = Actor::Human;
        let hide = |id, key| set(vec![change(id, key, Value::Bool(true))]);
        let mut cases = Vec::new();
        let mut docs = Vec::new();
        for (key, explicit, creation, project, vandalism) in [
            (field::DELETED, true, false, false, false),
            (field::DELETED, false, true, false, false),
            (field::ARCHIVED, true, false, false, false),
            (field::DELETED, true, false, true, false),
            (field::DELETED, false, false, true, false),
            (field::DELETED, true, false, false, true),
            (field::DELETED, false, false, false, true),
        ] {
            let fields = if explicit { vec![(key.clone(), Value::Bool(false))] } else { vec![] };
            let id = log.add(0, human, entity(if project { kind::PROJECT } else { kind::TASK }, fields));
            let hidden = if creation { Content::Undo { of: id } } else { hide(id, key.clone()) };
            let first = log.add(usize::from(vandalism), human, hidden);
            log.add(usize::from(!vandalism), human, Content::Undo { of: first });
            let second = log.add(0, human, hide(id, key));
            log.add(0, human, Content::Undo { of: second });
            cases.push((id, !vandalism));
            if project {
                for kind in [kind::TASK, kind::DOC] {
                    let child = log.add(0, human, Content::Entity { kind: Key::Number(kind), project: Some(id),
                        fields: vec![], derive: None, copied_from: vec![] });
                    cases.push((child, true));
                    if kind == kind::DOC { docs.push(child); }
                }
            }
        }
        let [a, b] = std::array::from_fn(|_| log.add(0, human, entity(kind::TASK, vec![])));
        let merge = |from, to| set(vec![change(from, field::MERGED_INTO, Value::Id(to)),
            change(from, field::DELETED, Value::Bool(true))]);
        log.add(0, human, merge(a, b));
        let mut edge = log.add(0, human, merge(b, a));
        for who in [0, 1, 0, 0] { edge = log.add(who, human, Content::Undo { of: edge }); }
        cases.push((a, true));

        for removed in [false, true] {
            if removed { log.remove(1, ben); }
            let live = fold(&log.events);
            let restored = State::restore(&log.events, &live.checkpoint(&log.events)).unwrap();
            for state in [&live, &restored] {
                let gate = members(state);
                for &(id, restored_by_ben) in &cases {
                    let expected = if removed && restored_by_ben { Err(Withheld::External) }
                        else { Ok(Trust::Member { by_ai: false }) };
                    assert_eq!(gate.entity_visible(&id).map(|p| p.trust()), expected,
                        "entity visibility includes each contributing restorer");
                }
                for doc in &docs {
                    let expected = if removed { Err(Withheld::External) } else { Ok(Trust::Member { by_ai: false }) };
                    assert_eq!(gate.doc_full_text(doc).map(|p| p.trust()), expected,
                        "document text includes the project's contributing restorer");
                }
            }
        }
    }

    #[test]
    fn gate_rules_enforced_across_classes_provenance_merges_and_policies() {
        doc_break_and_cleared_attr_origins_gate_empty_blocks_and_full_text();
        doc_checkbox_follows_linked_task_and_unlink_copy_provenance();
        entity_resurrection_project_and_merge_cycle_provenance_withheld();
        proposed_and_text_fields_keep_undo_chain_provenance();
        cleared_entity_field_withholds_instead_of_missing_source();
        board_item_placement_gates_deleted_sections_and_parent_cycles();
        session_summary_ignores_withheld_joins_leaves_declines_and_said();
        let mut log = TownLog::new();
        let ana = log.keys[0].public();
        let ben = log.keys[1].public();
        let kit = log.keys[2].public();
        let ben_seat = log.admit(1);
        let kit_seat = log.admit(2);
        let human = Actor::Human;
        let chat = log.add(1, human, Content::Chat(Chat { text: "Ben".into(), ..Chat::default() }));
        let profile = |name: &str| Content::Profile { name: name.into(), colour: 0 };
        let only_ben = log.add(1, human, profile("Ben"));
        log.add(0, human, profile("Lu"));
        let latest_ana = log.add(0, human, profile("Ana"));
        let title_of_chat = log.add(0, human, copy_record(chat));

        let open = |session| Content::Session(SessionOp::Open { session, to: vec![], town_wide: true, mode: SessionMode::Call { video: false } });
        let say = |session, text: &str| Content::Said { session, lines: vec![SaidLine { at_ms: 0, dur_ms: 1, text: text.into() }] };
        let request = |job, session| Content::Request { job, session, service: Service::Tasks, project: None, provider: ana };
        let done = |job| Content::Done { job, text: "Lu".into() };
        let session = [1; 16];
        let job = [7; 16];
        let failed_job = [8; 16];
        let open_ana = log.add(0, human, open(session));
        let open_ben = log.add(1, human, open([2; 16]));
        let ana_anchor_copy = log.add(0, human, copy_record(open_ana));
        let ben_anchor_copy = log.add(0, human, copy_record(open_ben));
        log.add(1, human, Content::Session(SessionOp::Join { session }));
        log.add(2, human, Content::Session(SessionOp::Join { session }));
        let pre = log.add(1, human, say(session, "Ben"));
        let said_copy = log.add(0, human, copy_record(pre));
        let req = log.add(0, human, request(job, session));
        log.add(0, human, request(failed_job, session));
        log.add(2, human, say(session, "Kit"));
        let winning = log.add(0, human, done(job));
        let superseded = log.add(0, human, done(job));
        let failed = log.add(0, human, Content::Failed { job: failed_job, why: Failure::Interrupted });
        let reports = [winning, superseded, failed].map(|id| log.add(0, human, copy_record(id)));
        let proposal = log.add(0, Actor::Ai, copy_record(req));
        let later_job = [9; 16];
        let later_session = [3; 16];
        log.add(0, human, open(later_session));
        log.add(0, human, say(later_session, "Ana"));
        log.add(0, human, request(later_job, later_session));
        log.add(0, human, done(later_job));

        let orphan = log.add(0, human, done([10; 16]));
        let no_open = log.add(0, human, request([11; 16], [4; 16]));
        let missing = [[0xfe; 32], orphan, no_open].map(|id| log.add(0, human, copy_record(id)));
        let seat_edit = log.add(0, human, entity(kind::APP_EDIT, vec![
            (field::EDIT_SEAT, Value::Key(ben.to_vec())), (field::TEXT, Value::Text("Ben".into()))]));
        let spoof = log.add(0, human, entity(kind::APP_EDIT, vec![
            (field::EDIT_SEAT, Value::Key(ben.to_vec())), (field::TEXT, Value::Text("Rik".into()))]));
        log.add(2, human, set(vec![change(spoof, field::EDIT_SEAT, Value::Key(ana.to_vec()))]));

        let app = log.add(2, human, entity(kind::APP, vec![]));
        let audit_edits = [(field::EDIT_AGENT, Value::Id(proposal)), (field::EDIT_AI, Value::Bool(true))].map(|(key, value)| {
            let edit = log.add(0, human, entity(kind::APP_EDIT, vec![
                (field::PARENT, Value::Id(app)), (field::EDIT_SEAT, Value::Key(ana.to_vec())),
                (key, value), (field::TEXT, Value::Text("Mira".into())),
            ]));
            log.add(0, human, set(vec![change(edit, field::PARENT, Value::Null), change(edit, field::EDIT_AGENT, Value::Null),
                change(edit, field::EDIT_AI, Value::Null), change(edit, field::TEXT, Value::Text("Eve".into()))]));
            edit
        });

        let doc = log.add(1, human, entity(kind::DOC, vec![]));
        let ana_doc = log.add(0, human, entity(kind::DOC, vec![]));
        let run_doc = log.add(0, human, entity(kind::DOC, vec![]));
        let run = log.add(0, human, Content::Text { doc: run_doc, ops: vec![
            TextOp::Insert { anchor: BreakRef::Root, text: "Ana".into() }] });
        let derive = |doc, copied_from| {
            let at = DocState::new(doc).root_id();
            (Content::Entity { kind: Key::Number(kind::TASK), project: None, derive: Some((doc, at)),
                fields: vec![(field::TITLE, Value::Text("Mira".into()))], copied_from }, crate::entity::derived_task(doc, at))
        };
        let (conversion, from_ben_doc) = derive(doc, vec![]);
        log.add(0, human, conversion);
        let (conversion, by_kit) = derive(ana_doc, vec![]);
        log.add(2, human, conversion);
        let (conversion, run_copy) = derive(run_doc, vec![reference(None, field::TITLE, CopySource::DocLine { doc: run_doc, events: vec![run] })]);
        log.add(0, human, conversion);

        let merge = |src, dst, writer| Content::Set {
            changes: vec![change(dst, field::TEXT, Value::Text("Kit".into())), change(src, field::DELETED, Value::Bool(true)),
                change(src, field::MERGED_INTO, Value::Id(dst))], label: None,
            copied_from: vec![reference(Some(dst), field::TEXT, CopySource::EntityField { entity: src, field: field::TITLE, writer })],
        };
        let survivor = log.add(0, human, entity(kind::TASK, vec![]));
        let src = log.add(0, human, entity(kind::TASK, vec![]));
        let writer = log.add(0, human, set(vec![change(src, field::TITLE, Value::Text("Lu".into()))]));
        let dst = log.add(0, human, entity(kind::TASK, vec![]));
        log.add(0, human, merge(src, dst, writer));
        let gh = log.add(0, human, entity(kind::TASK, (40..=44).map(|n|
            (Key::Number(n), if n == 41 { Value::Int(1) } else { Value::Text("Nora".into()) })).collect()));
        let open_key = Key::Name("accent".into());
        let town = log.add(0, human, entity(kind::TOWN, vec![(open_key.clone(), Value::Literal("[4  2] // note".into()))]));

        let state = fold(&log.events);
        let gate = members(&state);
        assert!(gate.is_bound_to(&state) && !gate.is_bound_to(&state.clone()), "the gate is bound to one State instance");
        assert_eq!(gate.redacted_header_visible(&chat), Err(Withheld::Hidden), "a readable message cannot use the redacted placeholder");
        let name = gate.open_key_name(&town, &open_key).unwrap();
        let lit = gate.entity_field_value(&town, &open_key).unwrap().as_literal().unwrap();
        let mut open = AiTextBuilder::new();
        open.push_splash_open_field(name.as_for_ai_str(), lit);
        assert_eq!(gate.finish_text(open).unwrap().as_str(), " accent: [4  2] // note\n", "an open literal with a trailing comment terminates before the next token");
        let check_len = |value: AiJson| {
            assert_eq!(value.encoded_len(), gate.finish_json(value).unwrap().as_str().len(),
                "ai_json_encoded_len_and_fragment_chunks_match_rendered_bytes");
        };
        for value in [AiJson::null(), AiJson::bool(false), AiJson::bool(true), AiJson::object(vec![]),
            AiJson::array(vec![]), AiJson::id_object(vec![]), AiJson::static_str("\"\n\\\t\r\x08\x0c\x01雪")] {
            check_len(value);
        }
        for n in [i64::MIN, 0, i64::MAX] { check_len(AiJson::int(n)); }
        for n in [1.0, -0.0, 2.5e-200, f64::MAX, f64::NAN, f64::INFINITY] { check_len(AiJson::f64(n)); }
        let ai_text = gate.entity_field_text(&proposal, &field::TEXT).unwrap();
        let long_text = gate.share_peer_text(&ana, "雪\"\n".repeat(3600)).unwrap();
        let row = AiJson::id_object(vec![(proposal, AiJson::object(vec![
            ("\t雪", AiJson::array(vec![AiJson::text(ai_text), AiJson::text(long_text)])),
            ("id", AiJson::revision(&proposal)),
        ]))]);
        let rendered = gate.finish_json(row.clone()).unwrap();
        assert_eq!(row.encoded_len(), rendered.as_str().len(), "nested row length includes escaped keys, separators, and seat ids");
        let builder = row.into_text_builder();
        let mut reassembled = String::new();
        // Reserve both string delimiters before budgeting the escaped payload.
        let budget = 256;
        loop {
            let (part, len, complete) = builder.json_chunk_to_json(reassembled.len(), budget - 2).unwrap();
            let out = gate.finish_json(part).unwrap();
            assert_eq!(out.trust(), Trust::Member { by_ai: true }, "each JSON fragment preserves the row's AI provenance");
            assert!(out.as_str().len() <= budget && len > 0, "each fragment makes progress within the wire budget");
            let decoded = json::parse(out.as_str().as_bytes()).unwrap();
            let piece = decoded.as_str().unwrap();
            assert_eq!(piece.len(), len, "fragment offset counts unescaped UTF-8 bytes");
            reassembled.push_str(piece);
            if complete { break }
            let next = rendered.as_str()[reassembled.len()..].chars().next().unwrap();
            assert!(out.as_str().len() + escaped_width(next) > budget, "the chunk is the longest fitting UTF-8 prefix");
        }
        assert_eq!(reassembled, rendered.as_str(), "JSON fragments reassemble the exact oversized signed row");
        assert_eq!(gate.chat_text(&chat).unwrap().value, "Ben", "active member chat passes");
        assert_eq!(Gate::new(&state, LocalPolicy::Cleared).chat_text(&chat), Err(Withheld::RequiresClearance),
            "local_cleared_override_withholds_uncleared_member");
        for n in 40..=46 {
            assert_eq!(entity_field_class(&Key::Number(kind::TASK), &Key::Number(n)), Class::External, "GH field {n} is always external");
            if n <= 44 { assert_eq!(gate.entity_field_value(&gh, &Key::Number(n)), Err(Withheld::External), "stored GH field {n} is withheld"); }
        }
        let mut intake = state.clone();
        intake.set_seat_role_for_test(&ben_seat, 1);
        assert_eq!(members(&intake).chat_text(&chat), Err(Withheld::External), "intake authors are external");
        let mut unknown = state.clone();
        unknown.unknown_critical.push([0xfe; 32]);
        assert_eq!(members(&unknown).chat_text(&chat), Err(Withheld::TownUnknownCritical), "unknown_critical_withholds_all");
        assert_eq!(members(&unknown).ack("{}"), Err(Withheld::TownUnknownCritical), "unknown critical records withhold even acknowledgements");
        let mut policy = state.clone();
        policy.receive(&log.event(0, human, set(vec![change(town, field::POLICY, Value::Int(2))]), true));
        assert_eq!(members(&policy).chat_text(&chat), Err(Withheld::TownPolicyUnreadable), "founder-critical unreadable_policy_withholds_all");

        assert_eq!(state.service_job_meta(&job).unwrap().report_writer, Some(winning), "first Done is the winning report");
        assert_eq!(gate.service_done(&job).unwrap().value, "Lu", "active transcript permits direct Done");
        for copy in reports { assert!(gate.entity_field_text(&copy, &field::TITLE).is_ok(), "winning, superseded and failed copies walk an active transcript"); }
        assert!(gate.entity_visible(&seat_edit).is_ok(), "original active APP_EDIT seat permits the entity");
        for edit in audit_edits {
            assert_eq!(gate.entity_visible(&edit).map(|v| v.trust()), Ok(Trust::Member { by_ai: true }),
                "cleared_app_edit_fields_keep_signed_parent_and_ai_provenance");
            let text = gate.created_entity_field_text(&edit, &field::TEXT).unwrap();
            assert_eq!((text.value.as_str(), text.trust()), ("Mira", Trust::Member { by_ai: true }),
                "the signed edit text and its AI provenance survive a Set");
        }
        assert!(gate.entity_visible(&spoof).is_ok(), "changing EDIT_SEAT leaves the active signed seat in force");
        assert!(gate.entity_field_text(&spoof, &field::TEXT).is_ok(), "the active signed seat authorizes the edit text");
        let mut hidden_app = state.clone();
        hidden_app.receive(&log.event(0, human, set(vec![change(app, field::DELETED, Value::Bool(true))]), false));
        for edit in audit_edits {
            assert_eq!(members(&hidden_app).entity_visible(&edit), Err(Withheld::Hidden), "clearing PARENT never detaches an edit from a deleted app");
            assert_eq!(members(&hidden_app).created_entity_field_text(&edit, &field::TEXT), Err(Withheld::Hidden),
                "the signed edit text is hidden with its original parent");
        }
        for task in [from_ben_doc, by_kit] { assert!(gate.entity_visible(&task).is_ok(), "live derived task resolves to its creator"); }
        let mut hidden_doc = state.clone();
        hidden_doc.receive(&log.event(0, human, set(vec![change(doc, field::DELETED, Value::Bool(true))]), false));
        assert_eq!(members(&hidden_doc).entity_visible(&from_ben_doc), Err(Withheld::Hidden), "pre-T2 derived task follows source doc visibility");
        for copy in missing {
            assert_eq!(gate.entity_field_text(&copy, &field::TITLE), Err(Withheld::MissingSource), "missing record, request or open withholds the copied field");
        }
        assert!(gate.entity_field_text(&dst, &field::TEXT).is_ok(), "a tombstone written by this merge permits its copy");
        let mut deleted = state.clone();
        deleted.receive(&log.event(0, human, set(vec![change(src, field::DELETED, Value::Bool(true))]), false));
        assert_eq!(members(&deleted).entity_field_text(&dst, &field::TEXT), Err(Withheld::Hidden), "an independent delete withholds the merged copy");

        // A signed event cannot name its own hash. Exercise the fold walk on an explicitly
        // cyclic projection, keeping this synthetic id out of the signed-history scenarios.
        let cycle_id = [0xfd; 32];
        let mut self_copy = log.event(0, human, Content::Set { changes: vec![change(dst, field::TITLE, Value::Text("Ana".into()))],
            label: None, copied_from: vec![reference(Some(dst), field::TITLE,
                CopySource::EntityField { entity: dst, field: field::TITLE, writer: cycle_id })] }, false);
        self_copy.id = cycle_id;
        let mut cycle = state.clone();
        cycle.receive(&self_copy);
        assert_eq!(members(&cycle).entity_field_text(&dst, &field::TITLE), Err(Withheld::Hidden),
            "a self-copy cannot name a writer in its own strict past");

        let name = |s: &State, key: &PublicKey| members(s).profile_or_fallback_name(key, Some("Rik"), &ana, Some("Mira")).unwrap().value;
        let neutral = |key: &PublicKey| format!("Member {}", hex(&key[..4]));
        assert_eq!(name(&state, &kit), "Rik", "a peer with no profile may use the pairing label");
        let mut with_void = state.clone();
        with_void.receive(&log.event(2, Actor::Ai, profile("Eve"), false));
        assert_eq!(name(&with_void, &kit), neutral(&kit), "a void-only profile suppresses the pairing label");
        with_void.receive(&log.event(1, Actor::Ai, profile("Eve"), false));
        assert_eq!(name(&with_void, &ben), "Ben", "a readable human profile outranks a void AI profile");
        let erased = [open_ben, superseded, chat, writer, run, pre, only_ben, latest_ana];
        let redacted: Vec<_> = log.events.iter().map(|e| if erased.contains(&e.id) {
            Event::from_parts(e.town, e.header_bytes.clone(), Vec::new(), e.sig.clone()).unwrap()
        } else { e.clone() }).collect();
        let redacted_state = fold(&redacted);
        let redacted_gate = members(&redacted_state);
        assert!(redacted_gate.redacted_header_visible(&chat).is_ok(), "active members can read a redacted header");
        assert_eq!(Gate::new(&redacted_state, LocalPolicy::Cleared).redacted_header_visible(&chat), Err(Withheld::RequiresClearance),
            "a redacted header cannot bypass local clearance");
        for id in erased {
            let meta = redacted_state.record_meta(&id).unwrap();
            assert!(!meta.void && !meta.readable, "redacting the body preserves a non-void signed header");
            assert_eq!(redacted_gate.record_visible(&id), Err(Withheld::Hidden), "every body-redacted record kind is hidden");
        }
        for (copy, field) in [(ben_anchor_copy, field::TITLE), (reports[1], field::TITLE), (title_of_chat, field::TITLE),
            (dst, field::TEXT), (run_copy, field::TITLE), (said_copy, field::TITLE), (proposal, field::TITLE)] {
            assert_eq!(redacted_gate.entity_field_text(&copy, &field), Err(Withheld::Hidden), "each copied redacted source is hidden");
        }
        assert_eq!(redacted_gate.chat_text(&chat), Err(Withheld::Hidden), "direct redacted chat is hidden");
        assert_eq!(redacted_gate.said_line(&session, &pre, "Ben".into()), Err(Withheld::Hidden), "direct redacted Said is hidden");
        assert_eq!(redacted_gate.service_done(&job), Err(Withheld::Hidden), "an unreadable fact in the request window withholds Done");
        assert!(redacted_gate.service_done(&later_job).is_ok(), "an unreadable fact before the next open leaves its report readable");
        let restored = State::restore(&redacted, &redacted_state.checkpoint(&redacted)).unwrap();
        for projection in [&redacted_state, &restored] {
            assert_eq!(name(projection, &ben), neutral(&ben), "redacted_profile_never_falls_back_to_pairing_label");
            assert_eq!(name(projection, &ana), neutral(&ana), "a redacted latest profile suppresses the stale profile and local chosen name");
        }
        assert_eq!(members(&restored).service_done(&job), Err(Withheld::Hidden), "restoring the checkpoint rebuilds unreadable session facts");

        log.remove(2, kit_seat);
        let kit_removed = fold(&log.events);
        for edit in audit_edits {
            assert_eq!(members(&kit_removed).entity_visible(&edit), Err(Withheld::External), "the signed parent creator contributes to edit trust");
        }
        assert_eq!(members(&kit_removed).entity_visible(&by_kit), Err(Withheld::External), "removing the derived task creator withholds it");
        for field in [field::TITLE, field::TEXT] {
            assert_eq!(members(&kit_removed).entity_field_text(&proposal, &field).unwrap().trust(), Trust::Member { by_ai: true },
                "post-request Said cannot taint a proposal and its AI label survives");
        }
        log.remove(1, ben_seat);
        let mut removed_redacted = redacted_state;
        for event in &log.events[redacted.len()..] { removed_redacted.receive(event); }
        assert_eq!(members(&removed_redacted).redacted_header_visible(&chat), Err(Withheld::External), "a removed author's header is withheld");
        let mut removed = fold(&log.events);
        let gate = members(&removed);
        assert_eq!(gate.entity_visible(&spoof), Err(Withheld::External), "overwriting EDIT_SEAT cannot replace a removed signed seat");
        assert_eq!(gate.entity_field_text(&spoof, &field::TEXT), Err(Withheld::External), "the removed signed seat withholds fields too");
        assert_eq!(gate.chat_text(&chat), Err(Withheld::External), "removed authors are external");
        assert_eq!(gate.entity_visible(&from_ben_doc), Err(Withheld::External), "removing the derived doc author withholds the task");
        assert_eq!(gate.entity_visible(&seat_edit), Err(Withheld::External), "removing the original EDIT_SEAT withholds the edit");
        assert_eq!(gate.entity_field_text(&seat_edit, &field::TEXT), Err(Withheld::External), "removing EDIT_SEAT withholds edit fields");
        let label = gate.profile_or_fallback_name(&ben, Some("Kit"), &ana, Some("Ana")).unwrap();
        assert_eq!(label.value, format!("Member {}", hex(&ben[..4])), "pairing_fallback_never_overrides_withheld_profile");
        assert_eq!(gate.service_done(&job), Err(Withheld::External), "removed pre-request speaker withholds direct Done");
        assert_eq!(gate.service_failure_note(&failed, "Lu".into()), Err(Withheld::External), "failure note follows its transcript too");
        for copy in reports { assert_eq!(gate.entity_field_text(&copy, &field::TITLE), Err(Withheld::External), "all reports follow the pre-request speaker"); }
        for field in [field::TITLE, field::TEXT] {
            assert_eq!(gate.entity_field_text(&proposal, &field), Err(Withheld::External), "pre-request Said withholds proposal title and text");
        }
        assert!(gate.record_visible(&open_ana).is_ok() && gate.entity_field_text(&ana_anchor_copy, &field::TITLE).is_ok(),
            "a session open checks the opener only");
        assert_eq!(gate.entity_field_text(&ben_anchor_copy, &field::TITLE), Err(Withheld::External), "removed opener withholds copied anchor");
        removed.receive(&log.event(0, human, merge(proposal, survivor, proposal), false));
        assert_eq!(members(&removed).entity_field_text(&survivor, &field::TEXT), Err(Withheld::External), "merging a proposal preserves request provenance");
    }
}
