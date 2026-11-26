//! Worker-side inspection of a selection. The file boundary fixes which calls, outcomes,
//! actor joins and effects existed when a continued copy began.
use super::*;
use crate::journal::replay::{apply, Positions};
use std::borrow::Cow;
use fab::ui::journal_panel::{DiffRowKind, DiffRowSpec, JournalDiff, JournalDiffUpdate};
use makepad_git::diff::{diff_lines_bounded, DiffOp};
use std::collections::BTreeSet;

fn field<'a>(row: &'a Value, key: &str) -> io::Result<&'a Value> { row.get(key).ok_or_else(invalid) }
fn string<'a>(row: &'a Value, key: &str) -> io::Result<&'a str> { field(row, key)?.as_str().ok_or_else(invalid) }
fn array<'a>(row: &'a Value, key: &str) -> io::Result<&'a [Value]> { field(row, key)?.as_arr().ok_or_else(invalid) }

struct Selection<'a> {
    index: &'a Index,
    file: BufReader<File>,
    dir: PathBuf,
    end: u64,
    positions: Option<Arc<Positions>>,
    current: &'a dyn Fn() -> bool,
}

impl Browse<'_> {
    fn selection(&self, end: u64) -> io::Result<Selection<'_>> {
        if end > self.complete_end() { return Err(invalid()) }
        let index = self.store.folders.get(&self.target.folder).ok_or_else(invalid)?;
        Ok(Selection {
            index,
            file: BufReader::new(index.open_log(self.root, &self.target.folder)?),
            dir: self.root.join(&*self.target.folder).join("journal"), end, positions: None, current: self.current,
        })
    }

    pub fn range(&self, from: &Key, to: &Key, filter: Filter, end: u64) -> io::Result<Vec<Position>> {
        if from.folder != self.target.folder || to.folder != from.folder || end > self.complete_end() { return Err(invalid()) }
        let index = self.store.folders.get(&from.folder).ok_or_else(invalid)?;
        for key in [from, to] {
            if Some(key.journal) != index.token() || !index.entries.get(&key.position).is_some_and(|e| e.offset < end) { return Err(io::ErrorKind::NotFound.into()) }
        }
        let mut positions = Vec::new();
        for (&pos, entry) in index.entries.range(from.position.min(to.position)..=from.position.max(to.position)) {
            if !(self.current)() { return Err(io::ErrorKind::Interrupted.into()) }
            if entry.offset >= end || filter.kind.is_some_and(|kind| entry.kind != kind) { continue }
            let actor = if entry.kind == Kind::Done {
                entry.call.and_then(|id| index.calls.get(&id.get()))
                    .and_then(|(p, _)| index.entries.get(p)).filter(|e| e.offset < end).and_then(|e| e.own_actor)
            } else { entry.own_actor };
            if filter.actor.is_none_or(|a| actor == Some(a)) { positions.push(pos); }
        }
        Ok(positions)
    }

    pub fn script(&self, from: &Key, to: &Key, filter: Filter, end: u64) -> io::Result<String> {
        let positions = self.range(from, to, filter, end)?;
        let mut selection = self.selection(end)?;
        let mut lines = BTreeMap::<Position, Vec<String>>::new();
        let mut calls = HashMap::new();
        for pos in positions {
            let mut row_lines = Vec::new();
            if let Err(err) = selection.copy_record(pos, &mut calls, &mut lines, &mut row_lines) {
                if err.kind() == io::ErrorKind::Interrupted { return Err(err) }
                row_lines.push(format!("# {}", problem(&err)));
            }
            lines.entry(pos).or_default().extend(row_lines);
        }
        Ok(lines.into_values().flatten().collect::<Vec<_>>().join("\n"))
    }

    pub fn diffs(&self, pos: Position, shown: Option<&Arc<DiffVersion>>) -> io::Result<(Arc<DiffVersion>, Option<Vec<JournalDiffUpdate>>)> {
        let mut selection = self.selection(self.complete_end())?;
        let index = selection.index;
        let entry = index.entries.get(&pos).ok_or_else(invalid)?;
        let effects = match entry.kind {
            Kind::Effect => std::slice::from_ref(&pos),
            Kind::Call | Kind::Done => entry.call.and_then(|id| index.effects.get(&id.get())).map_or(&[][..], Vec::as_slice),
            _ => &[],
        };
        let shown = shown.filter(|v| v.record == pos);
        if let Some(v) = shown {
            if Arc::ptr_eq(&v.prefix, &index.verified_prefix) && effects.len() == v.effects {
                return Ok((v.clone(), None))
            }
        }

        // Arrival order may differ from creation order. Each file spans its first
        // through its last effect, including any intermediate verified edits.
        let mut spans = BTreeMap::new();
        for &effect in effects {
            for name in changed_names(&selection.row(effect)?)? {
                let (first, last) = spans.entry(name.to_owned()).or_insert((effect, effect));
                *first = (*first).min(effect);
                *last = (*last).max(effect);
            }
        }
        let mut updates = Vec::new();
        let mut documents = BTreeMap::new();
        for (name, (first, last)) in spans {
            let ends = selection.revision(first, &name).and_then(|before| {
                let after = if last == first { before.clone() } else { selection.revision(last, &name)? };
                Ok((before, after))
            });
            let fingerprint = match &ends {
                Ok((first, last)) => Ok((first.before.as_deref().map(crate::journal::sha256_hex),
                                         last.after.as_deref().map(crate::journal::sha256_hex))),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(io::ErrorKind::Interrupted.into()),
                Err(e) => Err(e.kind()),
            };
            let keep = shown.and_then(|v| v.documents.get(&name)).filter(|(_, previous)| *previous == fingerprint);
            let update = if let Some((previous, _)) = keep {
                JournalDiffUpdate::Keep(*previous)
            } else {
                let diff = ends.and_then(|(first, last)| {
                    prepare_diff(&name, first.before.as_deref(), last.after.as_deref(), selection.current)
                });
                JournalDiffUpdate::Prepared(match diff {
                    Ok(diff) => diff,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
                    Err(e) => JournalDiff::note(format!("{name} · {}", problem(&e))),
                })
            };
            documents.insert(name, (updates.len(), fingerprint));
            updates.push(update);
        }
        let changed = shown.is_none_or(|v| v.documents != documents);
        let version = Arc::new(DiffVersion { record: pos, prefix: index.verified_prefix.clone(), effects: effects.len(), documents });
        Ok((version, changed.then_some(updates)))
    }
}

type Endpoints = Result<(Option<String>, Option<String>), io::ErrorKind>;

/// Describes exactly the diffs accepted by the panel. Ordinary appends only need
/// the prefix and linked-effect count. After a late insert, verify the endpoints
/// again before deciding whether an editor's displayed contents changed.
pub(crate) struct DiffVersion {
    record: Position,
    prefix: Arc<()>,
    effects: usize,
    documents: BTreeMap<String, (usize, Endpoints)>,
}

fn problem(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::NotFound => "Missing journal blob or record",
        _ => "Malformed record or gap in the document history",
    }
}

fn changed_names(row: &Value) -> io::Result<BTreeSet<&str>> {
    array(row, "changes")?.iter().map(|c| string(c, "name")).collect()
}

pub(super) struct CachedRevision {
    checkpoint: Position,
    position: Position,
    bytes: Arc<Revision>,
}

struct Revision {
    before: Option<Arc<[u8]>>,
    after: Option<Arc<[u8]>>,
}

fn text(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes).ok().filter(|s| !s.contains('\0'))
}

fn prepare_diff(name: &str, before: Option<&[u8]>, after: Option<&[u8]>, current: &dyn Fn() -> bool) -> io::Result<JournalDiff> {
    let (Some(before), Some(after)) = (before.and_then(text), after.and_then(text)) else {
        let operation = match (before, after) {
            (None, None) => "Created and removed", (None, Some(_)) => "Created",
            (Some(_), None) => "Removed", _ => "Binary change",
        };
        let hash = |bytes: Option<&[u8]>| bytes.map(crate::journal::sha256_hex).unwrap_or_else(|| "absent".into());
        return Ok(JournalDiff::note(format!("{operation} {name}\nBefore: {}\nAfter: {}", hash(before), hash(after))))
    };
    let diff = diff_lines_bounded(before, after, &Default::default(), &|| !current());
    if !current() { return Err(io::ErrorKind::Interrupted.into()) }
    let mut rows = Vec::new();
    let mut hunk = 0;
    let mut changed = false;
    for op in diff.ops {
        let equal = matches!(op, DiffOp::Equal { .. });
        if !equal && !changed { hunk += 1; }
        changed = !equal;
        let (kind, old, new, len) = match op {
            DiffOp::Equal { old_index, new_index, len } => (DiffRowKind::Equal, Some(old_index), Some(new_index), len),
            DiffOp::Delete { old_index, len } => (DiffRowKind::Removed, Some(old_index), None, len),
            DiffOp::Insert { new_index, len } => (DiffRowKind::Added, None, Some(new_index), len),
        };
        for line in 0..len {
            let number = |i: usize| u32::try_from(i + line).map_err(|_| invalid());
            rows.push(DiffRowSpec { kind, old_line: old.map(number).transpose()?, new_line: new.map(number).transpose()?, hunk: if equal { 0 } else { hunk } });
        }
    }
    let mut prepared = JournalDiff::prepare(name, &rows, before, after).map_err(|_| invalid())?;
    if diff.unavailable.is_some() {
        prepared.label.push_str(" · ");
        prepared.label.push_str(makepad_git::diff::Exhaustion::LABEL);
    } else if hunk == 0 && before != after {
        prepared.label.push_str(" · Line endings changed");
    }
    Ok(prepared)
}

impl Selection<'_> {
    fn visible(&self, pos: Position) -> bool { self.index.entries.get(&pos).is_some_and(|e| e.offset < self.end) }

    fn row(&mut self, pos: Position) -> io::Result<Value> {
        if !(self.current)() { return Err(io::ErrorKind::Interrupted.into()) }
        if !self.visible(pos) { return Err(io::ErrorKind::NotFound.into()) }
        self.index.record(&mut self.file, pos)
    }

    // Reuse the reader's durable index. Only loss markers need their payload to
    // locate a gap. Call arguments and replies are never read to prepare a diff.
    fn positions(&mut self) -> io::Result<Arc<Positions>> {
        if let Some(positions) = &self.positions { return Ok(positions.clone()) }
        let mut positions = Positions::default();
        for (&pos, entry) in &self.index.entries {
            if !(self.current)() { return Err(io::ErrorKind::Interrupted.into()) }
            if entry.offset >= self.end { continue }
            match pos {
                Position::Diagnostic(at) => positions.damaged(at),
                Position::Order(order) => {
                    let lost = (entry.kind == Kind::Lost).then(|| self.row(pos)).transpose()?;
                    positions.record(order, entry.offset, entry.kind, lost.as_ref());
                }
            }
        }
        let positions = Arc::new(positions);
        self.positions = Some(positions.clone());
        Ok(positions)
    }

    fn revision(&mut self, pos: Position, name: &str) -> io::Result<Arc<Revision>> {
        let Position::Order(order) = pos else { return Err(invalid()) };
        // A continued copy sees its original durable end. A late append can
        // change today's revision without changing the one that copy asked for.
        let latest = self.end == self.index.scanned - self.index.partial.len() as u64;
        let cached = latest.then(|| self.index.revisions.borrow().get(name)
            .map(|c| (c.checkpoint, c.position, c.bytes.clone()))).flatten();
        if let Some((_, previous, revision)) = &cached {
            if *previous == pos { return Ok(revision.clone()) }
        }
        let positions = self.positions()?;
        let walk = positions.through(Some(order));
        let checkpoint = Position::Order(walk.start);
        let mut files = BTreeMap::new();
        let mut last = None;
        let mut resume = None;
        if let Some((cp, Position::Order(previous), revision)) = cached {
            if cp == checkpoint && previous < order {
                last = revision.after.clone();
                if let Some(bytes) = &last { files.insert(name.to_owned(), bytes.clone()); }
                resume = Some(previous);
            }
        }
        let dir = self.dir.clone();
        let mut result = None;
        let stopped = walk.visit(&dir, Some(name), resume, &mut files,
            |n| self.row(Position::Order(n)).map(Cow::Owned),
            |step, row, files| {
                let after = files.get(name).cloned();
                if step.order == order && changed_names(row)?.contains(name) {
                    result = Some(Arc::new(Revision { before: last.take(), after: after.clone() }));
                }
                last = after;
                Ok(None)
            })?;
        if stopped.is_some() { return Err(invalid()) }
        let revision = result.ok_or_else(invalid)?;
        if latest {
            self.index.revisions.borrow_mut().insert(name.to_owned(), CachedRevision { checkpoint, position: pos, bytes: revision.clone() });
        }
        Ok(revision)
    }

    fn copy_record(&mut self, pos: Position, calls: &mut HashMap<u64, bool>, lines: &mut BTreeMap<Position, Vec<String>>, out: &mut Vec<String>) -> io::Result<()> {
        let row = self.row(pos)?;
        let kind = string(&row, "kind")?;
        let id = match kind { "call" | "done" => field(&row, "id")?.as_u64(), "effect" => row.get("call").and_then(Value::as_u64), _ => None };
        if let Some(id) = id {
            let replayed = if let Some(&replayed) = calls.get(&id) { replayed } else {
                let call_pos = self.index.calls.get(&id).map(|c| c.0).filter(|p| self.visible(*p));
                let call = call_pos.map(|p| self.row(p)).transpose()?;
                let done_pos = self.index.dones.get(&id).copied().filter(|p| self.visible(*p));
                let done = done_pos.map(|p| self.row(p)).transpose()?;
                let (line, replayed) = invocation(call.as_ref(), done.as_ref());
                lines.entry(call_pos.unwrap_or(pos)).or_default().push(line);
                calls.insert(id, replayed);
                replayed
            };
            if kind != "effect" || replayed { return Ok(()) }
        }
        match kind {
            "effect" | "history" | "reload" => {
                let changes = array(&row, "changes")?;
                if changes.is_empty() { out.push(format!("# {kind} with no document changes")); }
                // Keep recorded order, including multiple edits of the same file in one step.
                let mut files = BTreeMap::new();
                let mut seen = BTreeSet::new();
                let mut failed = BTreeSet::new();
                for change in changes {
                    let name = string(change, "name")?;
                    if seen.insert(name) {
                        match self.revision(pos, name) {
                            Ok(rev) => { if let Some(bytes) = &rev.before { files.insert(name.to_owned(), bytes.to_vec()); } }
                            Err(e) => {
                                if e.kind() == io::ErrorKind::Interrupted { return Err(e) }
                                out.push(format!("# {} for {}", problem(&e), s(name).to_json()));
                                failed.insert(name);
                            }
                        }
                    }
                    if failed.contains(name) { continue }
                    out.push(patch(&self.dir, &mut files, name, change)?);
                }
            }
            "checkpoint" => out.push("# Checkpoint · plate opened".into()),
            "lost" => out.push("# Lost journal records · recording gap".into()),
            "diagnostic" => out.push(format!("# Damaged journal line at byte {}", field(&row, "offset")?.to_json())),
            _ => out.push("# Malformed or unrecognised journal record".into()),
        }
        Ok(())
    }
}

/// The verified byte splice is also the one the script describes: reloads supply
/// a whole blob, ordinary text edits supply byte spans against the previous revision.
fn patch(dir: &Path, files: &mut BTreeMap<String, Vec<u8>>, name: &str, change: &Value) -> io::Result<String> {
    let old = files.get(name);
    let old_len = old.map(Vec::len);
    let old_binary = old.is_some_and(|b| text(b).is_none());
    apply(dir, files, change)?;
    let new = files.get(name);
    let before = field(change, "before")?;
    let after = field(change, "after")?;
    let operation = match (old_len, new) {
        (None, Some(_)) => "create", (_, None) => "remove", _ => "change",
    };
    if old_binary || new.is_some_and(|b| text(b).is_none()) {
        return Ok(format!("# Binary {operation} {} before {} after {}", s(name).to_json(), before.to_json(), after.to_json()));
    }
    let mut fields = vec![("doc", s(name)), ("before", before.clone())];
    match (old_len, new) {
        (_, None) => fields.push(("op", s("remove"))),
        (None, Some(bytes)) => {
            fields.push(("op", s("create")));
            fields.push(("text", s(text(bytes).ok_or_else(invalid)?)));
        }
        (Some(len), Some(bytes)) if change.get("blob").and_then(Value::as_bool) == Some(true) => {
            fields.push(("edits", Value::Arr(vec![obj(vec![("start", Value::Int(0)), ("end", Value::Int(len as i64)),
                ("text", s(text(bytes).ok_or_else(invalid)?))])])));
        }
        _ => fields.push(("edits", field(change, "edits")?.clone())),
    }
    Ok(format!("patch {}", obj(fields).to_json()))
}

fn invocation(call: Option<&Value>, done: Option<&Value>) -> (String, bool) {
    let Some(call) = call else { return ("# Missing captured call".into(), false) };
    let Some(outer) = call.get("action").and_then(Value::as_str) else { return ("# Malformed captured call".into(), false) };
    let args = call.get("args");
    let wrapper = fab_ai::ELEC_TOOLS.iter().any(|(tool, _, _)| *tool == crate::chat::short_tool(outer));
    let name = args.filter(|_| wrapper).and_then(|a| a.get("action")).and_then(Value::as_str).unwrap_or(outer);
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) {
        return (format!("# Malformed action name {}", s(name).to_json()), false)
    }
    match done.and_then(|r| r.get("outcome")).and_then(Value::as_str) {
        Some("ok") => (),
        Some(outcome @ ("refused" | "cancelled")) => return (format!("# {outcome} {name}"), false),
        None => return (format!("# {name} has no recorded outcome"), false),
        _ => return (format!("# Malformed outcome for {name}"), false),
    }
    if wrapper && name == outer { return (format!("# Malformed arguments for {outer}"), false); }
    let args = if wrapper {
        args.and_then(|v| v.get("args")).filter(|v| !v.is_null()).cloned().unwrap_or_else(|| obj(vec![]))
    } else if let Some(Value::Arr(pairs)) = args.filter(|_| name.starts_with("elec.")) {
        let mut fields = Vec::new();
        for pair in pairs {
            let (Ok(k), Ok(v)) = (string(pair, "name"), string(pair, "value")) else { return (format!("# Malformed arguments for {name}"), false) };
            if fields.iter().any(|(key, _)| key == k) { return (format!("# Duplicate argument for {name}"), false) }
            fields.push((k.to_string(), s(v)));
        }
        Value::Obj(fields)
    } else { args.cloned().unwrap_or(Value::Null) };
    if !matches!(args, Value::Obj(_)) { return (format!("# Malformed arguments for {name}"), false) }
    (format!("{name} {}", args.to_json()), true)
}
