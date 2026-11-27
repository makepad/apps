//! Blob references in one town's retained log. Membership replay can change the validity
//! of an old event, so the serve query consults the current fold for each witness.

use super::{id, Hash};
use crate::doc::TextOp;
use crate::entity::field;
use crate::event::{kind, Content, Event, Id, TownId, Value};
use crate::fold::State;
use crate::log::Log;
use std::collections::{HashMap, HashSet};

struct Referenced {
    root: Hash,
    size: u64,
    events: HashMap<usize, Id>,
}

impl Referenced {
    fn visible(&self, state: &State, send_end: Option<usize>) -> bool {
        self.events.iter().any(|(&position, event)|
            send_end.is_none_or(|end| position < end) && !state.void_events.contains_key(event))
    }
}

pub struct References {
    town: TownId,
    blobs: HashMap<Id, Referenced>,
    // A redaction can erase the only copy of the fields. Keep the ids to withdraw at that
    // position without needing its former body or scanning other events for the blob.
    by_position: HashMap<usize, Vec<Id>>,
}

fn blob(value: &Value) -> Option<(Id, u64)> {
    let Value::Blob { size, root } = value else { return None };
    Some((id(*size, root), *size))
}

impl References {
    pub fn from_log(log: &Log) -> Self {
        let mut refs = Self { town: log.town_id(), blobs: HashMap::new(), by_position: HashMap::new() };
        for (position, event) in log.events().iter().enumerate() {
            refs.record(position, event);
        }
        refs
    }

    /// Index an appended record, or revisit its position after a body is erased. Read the
    /// town's stored copy, which also supplies the arrival position used by sync.
    pub fn update(&mut self, log: &Log, position: usize) {
        if self.town == log.town_id() {
            if let Some(event) = log.events().get(position) { self.record(position, event); }
        }
    }

    fn record(&mut self, position: usize, event: &Event) {
        if let Some(previous) = self.by_position.remove(&position) {
            for id in previous {
                if let Some(reference) = self.blobs.get_mut(&id) {
                    reference.events.remove(&position);
                    if reference.events.is_empty() { self.blobs.remove(&id); }
                }
            }
        }

        let header = &event.header;
        if !matches!(header.kind, kind::ENTITY | kind::SET | kind::TEXT) { return; }
        let Some(bytes) = event.content() else { return };
        let Some(content) = Content::decode(header.kind, header.kind_version, bytes) else { return };
        let mut ids = Vec::new();
        let mut observe = |value: &Value| {
            if let Value::Blob { size, root } = *value {
                let id = id(size, &root);
                let reference = self.blobs.entry(id).or_insert_with(|| Referenced { size, root, events: HashMap::new() });
                if reference.events.insert(position, event.id).is_none() { ids.push(id); }
            }
        };
        match content {
            Content::Entity { fields, .. } => { for (_, value) in fields { observe(&value); } }
            Content::Set { changes, .. } => { for change in changes { observe(&change.value); } }
            Content::Text { ops, .. } => {
                for op in ops {
                    if let TextOp::Attr { value, .. } = op { observe(&value); }
                }
            }
            _ => {}
        }
        if !ids.is_empty() { self.by_position.insert(position, ids); }
    }

    pub fn servable(&self, state: &State, send_end: Option<usize>) -> HashMap<Id, u64> {
        self.blobs.iter().filter_map(|(id, reference)|
            reference.visible(state, send_end).then_some((*id, reference.size))).collect()
    }

    pub fn visible(&self, id: &Id, state: &State, send_end: Option<usize>) -> bool {
        self.blobs.get(id).is_some_and(|reference| reference.visible(state, send_end))
    }

    /// Signed descriptor used to begin a fetch in this town.
    pub fn descriptor(&self, id: &Id) -> Option<(u64, Hash)> {
        self.blobs.get(id).map(|r| (r.size, r.root))
    }

    pub fn reachable(&self, id: &Id) -> bool {
        self.blobs.contains_key(id)
    }

    /// Distinct blobs in the current fields of non-deleted entities, for fetch-at-once.
    pub fn current<'a>(&self, state: &'a State) -> impl Iterator<Item = (Id, u64)> + 'a {
        let mut seen = HashSet::new();
        state.entities.values().filter(|entity| !entity.flag(field::DELETED))
            .flat_map(|entity| entity.fields.values())
            .filter_map(|field| blob(&field.value))
            .filter(move |(id, _)| seen.insert(*id))
    }

    /// Select unreachable store filenames in input order. The caller owns directory I/O.
    pub fn sweep<S: AsRef<str> + Into<String>>(&self, names: impl IntoIterator<Item = S>) -> Vec<String> {
        names.into_iter().filter(|name| file_id(name.as_ref()).is_some_and(|id| !self.reachable(&id)))
            .map(Into::into).collect()
    }
}

fn file_id(name: &str) -> Option<Id> {
    let (hex, suffix) = name.split_at_checked(64)?;
    if !matches!(suffix, "" | ".part" | ".leaves" | ".tree" | ".tree.new")
        || !hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return None;
    }
    crate::parse_id(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::BreakRef;
    use crate::entity::kind as entity_kind;
    use crate::event::{Actor, Change, Key};
    use crate::fold::{fold, tests::Maker};
    use crate::log::tests::temp_dir;
    use crate::sync::send_end;
    use std::fs;

    fn value(n: u8) -> Value { Value::Blob { size: u64::from(n), root: [n; 32] } }
    fn blob_id(n: u8) -> Id { id(u64::from(n), &[n; 32]) }

    fn entity(fields: Vec<(Key, Value)>) -> Content {
        Content::Entity { kind: Key::Number(entity_kind::FILE), project: None, derive: None, fields }
    }

    #[test]
    fn redaction_deletion_and_field_replacement_have_distinct_reference_lifetimes() {
        let dir = temp_dir("blob-refs-history");
        let mut m = Maker::fresh(2, 1, false);
        let item = m.add(0, Actor::Human, entity(vec![(field::BLOB, value(1))]), None);
        let erased = m.add(0, Actor::Human, entity(vec![
            (field::BLOB, value(2)), (Key::Number(40), value(1)), (Key::Number(41), value(2)),
        ]), None);
        let mut original = Log::found(&dir.join("original"), m.events[0].clone()).unwrap();
        original.append_all(m.events[1..].to_vec()).unwrap();
        let mut refs = References::from_log(&original);
        assert_eq!(refs.current(&fold(original.events())).count(), 2, "duplicate field references fetch once");
        assert!(refs.reachable(&blob_id(2)));

        // Same local arrival positions, with one authenticated header retained after erasure.
        let mut log = Log::found(&dir.join("redacted"), m.events[0].clone()).unwrap();
        log.append(m.events[1].clone()).unwrap();
        let event = original.get(&erased).unwrap();
        log.append(Event::from_parts(log.town_id(), event.header_bytes.clone(), Vec::new(), event.sig.clone()).unwrap()).unwrap();
        refs.update(&log, log.position(&erased).unwrap());
        let mut state = fold(log.events());
        let x = blob_id(1);
        let hidden = blob_id(2);
        for index in [&refs, &References::from_log(&log)] {
            assert!(!index.reachable(&hidden), "last non-redacted witness disappeared");
            assert_eq!(index.servable(&state, None), HashMap::from([(x, 1)]), "the other event still names X");
            assert_eq!(index.current(&state).collect::<Vec<_>>(), [(x, 1)]);
            let gone = crate::hex(&hidden);
            let kept = crate::hex(&x);
            let files: Vec<_> = ["", ".part", ".leaves", ".tree", ".tree.new"].iter()
                .map(|suffix| format!("{gone}{suffix}")).collect();
            let mut names = files.clone();
            names.extend([kept.clone(), format!("{kept}.tree"), format!("{gone}."), format!("{gone}.tmp"),
                format!("{gone}.tree.new.extra"), format!("{gone}/file"), format!("{gone}/../{gone}"),
                format!(" {gone}"), gone.to_uppercase(), format!("{}é", &gone[..63]), "add-a.tmp".to_owned()]);
            assert_eq!(index.sweep(names), files, "only exact lowercase store names for unreachable blobs");
        }

        let overwrite = Content::Set { changes: vec![Change { entity: item, field: field::BLOB, value: value(3) }],
            label: None };
        let delete = Content::Set { changes: vec![Change { entity: item, field: field::DELETED, value: Value::Bool(true) }],
            label: None };
        let text = Content::Text { doc: item, ops: vec![TextOp::Attr { at: BreakRef::Root, attr: 99, value: value(4) }] };
        for (step, content) in [overwrite, delete, text].into_iter().enumerate() {
            m.add(0, Actor::Human, content, None);
            let event = m.events.last().unwrap();
            log.append(event.clone()).unwrap();
            state.receive(event);
            let pos = log.position(&event.id).unwrap();
            refs.update(&log, pos);
            let rebuilt = References::from_log(&log);
            for index in [&refs, &rebuilt] {
                let mut expected = HashMap::from([(x, 1), (blob_id(3), 3)]);
                if step == 2 { expected.insert(blob_id(4), 4); }
                assert_eq!(index.servable(&state, None), expected, "authorized history at step {step}");
                assert!(index.reachable(&x) && index.reachable(&blob_id(3)));
                assert!(!index.reachable(&hidden));
                let current: Vec<_> = index.current(&state).collect();
                if step == 0 { assert_eq!(current, [(blob_id(3), 3)], "overwritten blob leaves fetch-at-once"); }
                else { assert!(current.is_empty(), "deleted entity leaves fetch-at-once"); }
            }
        }
        drop((log, original));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn late_removal_restores_a_void_blob_witness_without_reindexing_it() {
        let dir = temp_dir("blob-refs-cutoff");
        let mut m = Maker::fresh(2, 3, false);
        let remover_seat = m.invite(0, 1);
        m.accept(1, remover_seat, None);
        let author_seat = m.invite(1, 2);
        let accepted = m.accept(2, author_seat, None);
        let membership = m.events.len();
        let witness = m.add(2, Actor::Human, entity(vec![(field::BLOB, value(7)), (Key::Number(40), value(8))]), None);
        // A second witness for blob 8 is above the first witness's send boundary. It stays
        // valid when the first is void, so neither witness alone then satisfies both rules.
        m.add(0, Actor::Human, entity(vec![(field::BLOB, value(8))]), Some(vec![author_seat]));
        let removal = m.remove(1, author_seat, 2, accepted, Some(vec![witness]));
        // The founder signed on an earlier frontier. Its cutoff preserves the remover's
        // invitation of the author, but voids the remover's later removal of that author.
        let late = m.remove(0, remover_seat, 1, author_seat, Some(vec![author_seat]));
        let mut log = Log::found(&dir.join("town"), m.events[0].clone()).unwrap();
        log.append_all(m.events[1..membership].to_vec()).unwrap();
        let mut refs = References::from_log(&log);
        let mut state = fold(log.events());
        let me = log.genesis().header.author;
        let x = blob_id(7);
        for event in &m.events[membership..] {
            log.append(event.clone()).unwrap();
            refs.update(&log, log.position(&event.id).unwrap());
            state.receive(event);
            let visible = !state.void_events.contains_key(&witness);
            assert_eq!(visible, event.id != removal);
            if event.id == late { assert!(state.void_events.contains_key(&removal), "the late cutoff wins"); }
            let author = log.get(&witness).unwrap().header.author;
            let end = send_end(&log, &state, &me, &author);
            assert_eq!(end, (!visible).then_some(membership + 3), "sync includes the counted removal itself");
            for index in [&refs, &References::from_log(&log)] {
                assert!(index.reachable(&x), "a void witness still keeps the town's files");
                assert!(index.sweep([crate::hex(&x)]).is_empty());
                assert!(index.servable(&state, Some(membership)).is_empty(), "the send end is exclusive");
                for bound in [Some(membership + 1), end, None] {
                    assert_eq!(index.servable(&state, bound).get(&x).copied(), visible.then_some(7));
                }
                assert_eq!(index.servable(&state, Some(membership + 1)).contains_key(&blob_id(8)), visible,
                    "one witness must be both valid and below the send end");
                assert!(index.servable(&state, None).contains_key(&blob_id(8)));
                assert_eq!(index.current(&state).any(|candidate| candidate == (x, 7)), visible);
            }
        }
        drop(log);
        fs::remove_dir_all(dir).unwrap();
    }
}
