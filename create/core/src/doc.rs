//! One replay-ordered sequence of Unicode scalars. A delimiter travels with its block.
use crate::entity::FieldState;
use crate::event::{Actor, Id, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CharId {
    pub event: Id,
    pub index: u32,
}

impl CharId {
    pub(crate) fn bytes(self) -> Vec<u8> {
        let mut bytes = self.event.to_vec();
        bytes.extend(self.index.to_le_bytes());
        bytes
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let (event, index) = bytes.split_at_checked(32)?;
        Some(Self { event: event.try_into().ok()?, index: u32::from_le_bytes(index.try_into().ok()?) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BreakRef {
    Root,
    Char(CharId),
    /// An attribute on a newline inserted earlier in this burst. Its event hash is not known yet.
    Inserted(u32),
}

pub type Anchor = BreakRef;

impl BreakRef {
    pub(crate) fn bytes(self) -> Vec<u8> {
        match self {
            Self::Root => vec![],
            Self::Char(id) => id.bytes(),
            Self::Inserted(i) => i.to_le_bytes().to_vec(),
        }
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes.len() {
            0 => Some(Self::Root),
            4 => Some(Self::Inserted(u32::from_le_bytes(bytes.try_into().ok()?))),
            _ => Some(Self::Char(CharId::decode(bytes)?)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextOp {
    Insert { anchor: Anchor, text: String },
    Delete { from: CharId, to: CharId },
    Attr { at: BreakRef, attr: u16, value: Value },
    Move { block: BreakRef, after: BreakRef },
}

pub mod attr {
    pub const KIND: u16 = 1;
    pub const INDENT: u16 = 2;
    pub const CHECKBOX: u16 = 3;
    pub const TASK: u16 = 4;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Node {
    run: usize,
    index: u32,
    // A Unicode scalar needs 21 bits. Keep the common one-tombstone case in the same word.
    scalar: u32,
}

impl Node {
    const HIDDEN: u32 = 1 << 31;

    fn ch(self) -> char { char::from_u32(self.scalar & !Self::HIDDEN).unwrap() }
    fn newline(self) -> bool { self.scalar & !Self::HIDDEN == '\n' as u32 }
    fn live(self) -> bool { self.scalar & Self::HIDDEN == 0 }

    fn visibility(&mut self, hide: bool, overlaps: &mut BTreeMap<(usize, u32), usize>) {
        let key = (self.run, self.index);
        if hide {
            if !self.live() { *overlaps.entry(key).or_default() += 1; }
            self.scalar |= Self::HIDDEN;
        } else if let Some(extra) = overlaps.get_mut(&key) {
            *extra -= 1;
            if *extra == 0 { overlaps.remove(&key); }
        } else {
            self.scalar &= !Self::HIDDEN;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    event: Id,
    positions: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Inverse {
    Visibility { run: usize, first: u32, last: u32, hide: bool },
    Attr { at: BreakRef, attr: u16, before: Value, before_writer: Option<Id>, before_origin: Option<Id>, before_source: Option<CheckboxSource> },
    Move { block: BreakRef, before: Option<BreakRef>, stop: Option<BreakRef> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopiedStatus {
    /// Copied from `task`'s `field::STATUS` at `writer` (an earlier write or undo).
    TaskStatus(Id),
    /// Copied from an earlier tick in the same `Text` event on `at` via `link_writer`.
    SameEventTick { at: BreakRef, link_writer: Id },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckboxSource {
    pub task: Id,
    pub link_writer: Id,
    pub status: Option<CopiedStatus>,
}

#[derive(Default)]
pub(crate) struct TaskEffects {
    pub writes: BTreeMap<Id, (Value, BreakRef, Id)>,
    pub links: BTreeSet<Id>,
}

pub(crate) fn checkbox(status: Option<Value>) -> Value {
    let state = match &status {
        Some(Value::Text(s)) if s == "done" || s == "review" => s.as_str(),
        _ => "open",
    };
    Value::Text(state.into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocState {
    nodes: Vec<Node>,
    runs: Vec<Run>,
    slots: BTreeMap<Id, usize>,
    overlaps: BTreeMap<(usize, u32), usize>,
    attributes: BTreeMap<(BreakRef, u16), FieldState>,
    inverses: BTreeMap<Id, Vec<Inverse>>,
    undone: BTreeSet<Id>,
    pub(crate) task_states: BTreeMap<Id, Value>,
    checkbox_sources: BTreeMap<BreakRef, CheckboxSource>,
}

impl DocState {
    pub fn new(doc: Id) -> Self {
        Self {
            nodes: vec![Node { run: 0, index: 0, scalar: '\n' as u32 }],
            runs: vec![Run { event: doc, positions: vec![0] }],
            slots: [(doc, 0)].into(), overlaps: BTreeMap::new(), attributes: BTreeMap::new(),
            inverses: BTreeMap::new(), undone: BTreeSet::new(), task_states: BTreeMap::new(), checkbox_sources: BTreeMap::new(),
        }
    }

    pub fn root_id(&self) -> CharId { CharId { event: self.runs[0].event, index: 0 } }

    /// Canonical block key: maps the root delimiter's `CharId` to `BreakRef::Root`.
    pub fn canonical(&self, at: BreakRef) -> BreakRef {
        match at {
            BreakRef::Char(id) if id == self.root_id() => BreakRef::Root,
            at => at,
        }
    }

    pub fn has_linked_tasks(&self) -> bool { !self.task_states.is_empty() }

    pub fn links_task(&self, task: &Id) -> bool { self.task_states.contains_key(task) }

    pub fn node_count(&self) -> usize { self.nodes.len() - 1 }

    pub fn text(&self) -> String {
        self.nodes.iter().skip(1).filter(|n| n.live()).map(|n| n.ch()).collect()
    }

    fn id(&self, n: Node) -> CharId { CharId { event: self.runs[n.run].event, index: n.index } }

    fn marker(&self, n: Node) -> BreakRef {
        if n.run == 0 { BreakRef::Root } else { BreakRef::Char(self.id(n)) }
    }

    fn pos(&self, id: CharId) -> Option<usize> {
        self.runs.get(*self.slots.get(&id.event)?)?.positions.get(id.index as usize).copied()
    }

    pub(crate) fn delimiter(&self, id: CharId) -> Option<BreakRef> {
        let node = self.nodes[*self.runs.get(*self.slots.get(&id.event)?)?.positions.get(id.index as usize)?];
        (node.ch() == '\n').then(|| self.marker(node))
    }

    fn position(&self, at: BreakRef) -> Option<usize> {
        match at {
            BreakRef::Root => self.runs.first()?.positions.first().copied(),
            BreakRef::Char(id) => self.pos(id),
            BreakRef::Inserted(_) => None,
        }
    }

    fn reindex(&mut self, start: usize, end: usize) {
        for (offset, node) in self.nodes[start..end].iter().enumerate() {
            self.runs[node.run].positions[node.index as usize] = start + offset;
        }
    }

    pub fn attribute_writer(&self, at: BreakRef, attr: u16) -> Option<Id> {
        let at = self.canonical(at);
        self.attributes.get(&(at, attr)).map(|f| f.writer)
    }

    /// Original authoring event of a live attribute, including after undo or redo.
    pub fn attribute_origin(&self, at: BreakRef, attr: u16) -> Option<Id> {
        let at = self.canonical(at);
        self.attributes.get(&(at, attr)).filter(|f| f.value != Value::Null).map(|f| f.origin)
    }

    /// Signed runs contributing live characters to this block, in event id order.
    pub fn block_run_events(&self, at: BreakRef) -> Option<Vec<Id>> {
        let at = self.canonical(at);
        let start = self.position(at)?;
        let delim = self.nodes.get(start)?;
        if !delim.newline() || (start != 0 && !delim.live()) { return None }
        let mut events = self.nodes[start + 1..self.end_of_block(start)].iter()
            .filter(|n| n.live()).map(|n| self.runs.get(n.run).map(|run| run.event)).collect::<Option<Vec<_>>>()?;
        events.sort_unstable();
        events.dedup();
        Some(events)
    }

    /// Distinct authoring events for the block's live attributes, in event id order.
    pub fn block_attr_origins(&self, at: BreakRef) -> Vec<Id> {
        let at = self.canonical(at);
        let mut origins: Vec<_> = self.attributes.range((at, 0)..=(at, u16::MAX))
            .filter(|(_, f)| f.value != Value::Null).map(|(_, f)| f.origin).collect();
        origins.sort_unstable();
        origins.dedup();
        origins
    }

    pub fn attribute(&self, at: BreakRef, attr: u16) -> Option<&Value> {
        let at = self.canonical(at);
        if attr == attr::CHECKBOX {
            if let Some(Value::Id(task)) = self.attribute(at, attr::TASK) {
                return self.task_states.get(task);
            }
        }
        self.attributes.get(&(at, attr)).map(|f| &f.value).filter(|v| **v != Value::Null)
    }

    fn replace_checkbox_source(&mut self, at: BreakRef, attr: u16, source: Option<CheckboxSource>) -> Option<CheckboxSource> {
        if attr != attr::CHECKBOX { return None }
        match source {
            Some(source) => self.checkbox_sources.insert(at, source),
            None => self.checkbox_sources.remove(&at),
        }
    }

    /// Source selected by the unlink that wrote the current unlinked checkbox.
    pub fn block_checkbox_source(&self, at: BreakRef) -> Option<CheckboxSource> {
        let at = self.canonical(at);
        if matches!(self.attribute(at, attr::TASK), Some(Value::Id(_))) { return None }
        self.attributes.get(&(at, attr::CHECKBOX)).filter(|f| f.value != Value::Null)?;
        self.checkbox_sources.get(&at).copied()
    }

    pub(crate) fn block_attr_writers(&self, at: BreakRef) -> Vec<(u16, Id)> {
        let at = self.canonical(at);
        self.attributes.range((at, 0)..=(at, u16::MAX)).map(|(&(_, key), f)| (key, f.writer)).collect()
    }

    pub(crate) fn attr_undo_prev(&self, undo: &Id, of: &Id, at: BreakRef, attr: u16) -> Option<Id> {
        let at = self.canonical(at);
        if !self.attr_written_by(undo, at, attr) { return None }
        self.inverses.get(of)?.iter().find_map(|inv| match inv {
            Inverse::Attr { at: a, attr: k, before_writer, .. } if *a == at && *k == attr => *before_writer,
            _ => None,
        })
    }

    /// State of `(at, attr)` immediately after the latest effective write in `in_past`'s causal past:
    /// `(writer, value, checkbox_source)` (`checkbox_source` is populated only when `attr == attr::CHECKBOX`).
    pub(crate) fn attr_state_before(
        &self,
        at: BreakRef,
        attr: u16,
        mut in_past: impl FnMut(Id) -> bool,
    ) -> Option<(Id, Value, Option<CheckboxSource>)> {
        let at = self.canonical(at);
        let fs = self.attributes.get(&(at, attr))?;
        let mut writer = fs.writer;
        let mut value = fs.value.clone();
        let mut source = (attr == attr::CHECKBOX)
            .then(|| self.checkbox_sources.get(&at).copied())
            .flatten();
        loop {
            if in_past(writer) {
                return Some((writer, value, source));
            }
            let inv = self.inverses.get(&writer)?.iter().find(|inv| {
                matches!(inv, Inverse::Attr { at: a, attr: k, .. } if *a == at && *k == attr)
            })?;
            let Inverse::Attr { before, before_writer, before_source, .. } = inv else { unreachable!() };
            writer = (*before_writer)?;
            value = before.clone();
            if attr == attr::CHECKBOX {
                source = *before_source;
            }
        }
    }

    /// True iff `event` recorded an effective `Inverse::Attr` on `(at, attr)`.
    pub(crate) fn attr_written_by(&self, event: &Id, at: BreakRef, attr: u16) -> bool {
        let at = self.canonical(at);
        self.inverses.get(event).is_some_and(|invs| {
            invs.iter().any(|inv| matches!(inv, Inverse::Attr { at: a, attr: k, .. } if *a == at && *k == attr))
        })
    }

    fn end_of_block(&self, start: usize) -> usize {
        self.nodes[start + 1..].iter().position(|n| n.newline() && n.live())
            .map_or(self.nodes.len(), |offset| start + 1 + offset)
    }

    fn move_block(&mut self, block: BreakRef, after: BreakRef) -> Option<Inverse> {
        let start = self.position(block)?;
        let end = self.end_of_block(start);
        let dest = self.end_of_block(self.position(after)?);
        self.splice_block(block, start, end, dest)
    }

    fn splice_block(&mut self, block: BreakRef, start: usize, end: usize, dest: usize) -> Option<Inverse> {
        if (start..=end).contains(&dest) { return None }
        // Remember both gaps. A later merge can hide either delimiter without destroying its id.
        let before = self.nodes.get(end).map(|n| self.marker(*n));
        let stop = self.nodes.get(dest).map(|n| self.marker(*n));
        let (lo, hi) = if dest < start {
            self.nodes[dest..end].rotate_right(end - start);
            (dest, end)
        } else {
            self.nodes[start..dest].rotate_left(end - start);
            (start, dest)
        };
        self.reindex(lo, hi);
        Some(Inverse::Move { block, before, stop })
    }

    /// Check every reference before mutating. Only attributes may name an earlier local newline.
    #[cfg(test)]
    pub(crate) fn apply(&mut self, event: Id, ops: &[TextOp], past: impl FnMut(Id) -> bool) {
        self.apply_event(event, ops, past, Actor::Human, |_| None);
    }

    pub(crate) fn apply_event(&mut self, event: Id, ops: &[TextOp], mut past: impl FnMut(Id) -> bool,
        actor: Actor, status: impl Fn(Id) -> Option<(Value, Id)>) -> TaskEffects {
        let mut tasks = TaskEffects::default();
        let mut count = 0usize;
        let mut new_breaks = BTreeSet::new();
        let mut known = |at: BreakRef, delimiter: bool| {
            self.position(at).is_some_and(|i| {
                let node = self.nodes[i];
                past(self.runs[node.run].event) && (!delimiter || node.ch() == '\n')
            })
        };
        for op in ops {
            let valid = match op {
                TextOp::Insert { anchor, text } => {
                    for ch in text.chars() {
                        let Ok(index) = u32::try_from(count) else { return tasks };
                        if ch == '\n' { new_breaks.insert(index); }
                        count += 1;
                    }
                    known(*anchor, false)
                }
                TextOp::Delete { from, to } => known(Anchor::Char(*from), false) && known(Anchor::Char(*to), false),
                TextOp::Attr { at: BreakRef::Inserted(i), .. } => new_breaks.contains(i),
                TextOp::Attr { at, .. } => known(*at, true),
                TextOp::Move { block, after } => known(*block, true) && known(*after, true),
            };
            if !valid { return tasks }
            if let TextOp::Attr { attr: attr::TASK, value: Value::Id(task), .. } = op {
                if status(*task).is_none() { return tasks }
            }
        }
        let run = self.runs.len();
        if count > 0 {
            self.runs.push(Run { event, positions: Vec::with_capacity(count) });
            self.slots.insert(event, run);
        }
        let mut inverse = Vec::new();
        let mut attr_writes = BTreeSet::new();
        for op in ops {
            match op {
                TextOp::Insert { anchor, text } if !text.is_empty() => {
                    let at = if *anchor == Anchor::Root { 1 } else { self.position(*anchor).unwrap() + 1 };
                    let first = self.runs[run].positions.len();
                    let new = text.chars().enumerate().map(|(i, ch)| Node { run, index: (first + i) as u32, scalar: ch as u32 });
                    let old_len = self.nodes.len();
                    self.nodes.splice(at..at, new);
                    let total = first + self.nodes.len() - old_len;
                    self.runs[run].positions.resize(total, 0);
                    self.reindex(at, self.nodes.len());
                    inverse.push(Inverse::Visibility { run, first: first as u32, last: (total - 1) as u32, hide: true });
                }
                TextOp::Insert { .. } => {}
                TextOp::Delete { from, to } => {
                    let (start, end) = (self.pos(*from).unwrap(), self.pos(*to).unwrap());
                    if start > end { continue }
                    // The leading structural delimiter does not represent a deletable character.
                    // A delete hides only what its writer had seen: text typed concurrently inside
                    // the range survives, as a concurrent edit is never dropped without a trace.
                    for n in &mut self.nodes[start.max(1)..=end] {
                        if n.run != run && !past(self.runs[n.run].event) { continue }
                        n.visibility(true, &mut self.overlaps);
                        if let Some(Inverse::Visibility { run, last, hide: false, .. }) = inverse.last_mut() {
                            if *run == n.run && last.checked_add(1) == Some(n.index) {
                                *last = n.index;
                                continue
                            }
                        }
                        inverse.push(Inverse::Visibility { run: n.run, first: n.index, last: n.index, hide: false });
                    }
                }
                TextOp::Attr { at, attr, value } => {
                    let at = match at {
                        BreakRef::Inserted(index) => BreakRef::Char(CharId { event, index: *index }),
                        at => self.canonical(*at),
                    };
                    if let Some((task, link_writer)) = self.attributes.get(&(at, attr::TASK))
                        .and_then(|f| match f.value { Value::Id(task) => Some((task, f.writer)), _ => None })
                    {
                        if *attr == attr::CHECKBOX {
                            if let Value::Text(wanted) = value {
                                let state = match wanted.as_str() {
                                    "done" if actor == Actor::Ai => "review",
                                    "done" => "done", "review" => "review", "open" => "open", _ => continue,
                                };
                                tasks.writes.insert(task, (Value::Text(state.into()), at, link_writer));
                            }
                            continue
                        }
                        if *attr == attr::TASK && *value == Value::Null {
                            let (before, copied) = if let Some((val, tick_at, tick_link_writer)) = tasks.writes.get(&task) {
                                (Some(val.clone()), Some(CopiedStatus::SameEventTick { at: *tick_at, link_writer: *tick_link_writer }))
                            } else if let Some((val, status_writer)) = status(task) {
                                (Some(val), Some(CopiedStatus::TaskStatus(status_writer)))
                            } else {
                                (None, None)
                            };
                            let source = Some(CheckboxSource { task, link_writer, status: copied });
                            self.write_attr(event, at, attr::CHECKBOX, checkbox(before), source, &mut inverse, &mut attr_writes);
                        }
                    }
                    if let (attr::TASK, Value::Id(task)) = (*attr, value) {
                        self.task_states.insert(*task, checkbox(status(*task).map(|(v, _)| v)));
                        tasks.links.insert(*task);
                    }
                    self.write_attr(event, at, *attr, value.clone(), None, &mut inverse, &mut attr_writes);
                }
                TextOp::Move { block, after } => {
                    if let Some(change) = self.move_block(*block, *after) { inverse.push(change); }
                }
            }
        }
        if !inverse.is_empty() { self.inverses.insert(event, inverse); }
        tasks
    }

    fn write_attr(&mut self, event: Id, at: BreakRef, attr: u16, value: Value, source: Option<CheckboxSource>,
        inverse: &mut Vec<Inverse>, written: &mut BTreeSet<(BreakRef, u16)>) {
        let before = self.attributes.insert((at, attr), FieldState { value, writer: event, origin: event });
        let before_source = self.replace_checkbox_source(at, attr, source);
        if written.insert((at, attr)) {
            let before_writer = before.as_ref().map(|s| s.writer);
            let before_origin = before.as_ref().map(|s| s.origin);
            inverse.push(Inverse::Attr { at, attr, before_writer, before_origin, before_source, before: before.map_or(Value::Null, |s| s.value) });
        }
    }

    pub(crate) fn undo(&mut self, event: Id, of: Id) {
        let Some(changes) = self.inverses.get(&of).cloned() else { return };
        if !self.undone.insert(of) { return }
        let mut inverse = Vec::new();
        for change in changes.into_iter().rev() {
            match change {
                Inverse::Visibility { run, first, last, hide } => {
                    for &pos in &self.runs[run].positions[first as usize..=last as usize] {
                        let n = &mut self.nodes[pos];
                        n.visibility(hide, &mut self.overlaps);
                    }
                    inverse.push(Inverse::Visibility { run, first, last, hide: !hide });
                }
                Inverse::Attr { at, attr, before, before_writer: _, before_origin, before_source } => {
                    let current = self.attributes.get_mut(&(at, attr)).unwrap();
                    if current.writer == of {
                        let prev_writer = std::mem::replace(&mut current.writer, event);
                        let before = std::mem::replace(&mut current.value, before);
                        let prev_origin = std::mem::replace(&mut current.origin, before_origin.unwrap_or(event));
                        let prev_source = self.replace_checkbox_source(at, attr, before_source);
                        inverse.push(Inverse::Attr { at, attr, before, before_writer: Some(prev_writer),
                            before_origin: Some(prev_origin), before_source: prev_source });
                    }
                }
                Inverse::Move { block, before, stop } => {
                    let start = self.position(block).unwrap();
                    let mut end = self.end_of_block(start);
                    if let Some(gap) = stop.and_then(|at| self.position(at)).filter(|gap| *gap > start) {
                        end = end.min(gap);
                    }
                    let dest = before.and_then(|at| self.position(at)).unwrap_or(self.nodes.len());
                    if let Some(change) = self.splice_block(block, start, end, dest) { inverse.push(change); }
                }
            }
        }
        if !inverse.is_empty() { self.inverses.insert(event, inverse); }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub at: BreakRef,
    pub text: String,
    pub attrs: BTreeMap<u16, Value>,
}

impl DocState {
    pub fn blocks(&self) -> Vec<Block> {
        self.blocks_in(&self.nodes)
    }

    fn blocks_in(&self, nodes: &[Node]) -> Vec<Block> {
        let mut blocks = Vec::new();
        for (pos, &node) in nodes.iter().enumerate() {
            if pos != 0 && !node.live() { continue }
            if node.ch() == '\n' {
                let at = self.marker(node);
                let mut attrs: BTreeMap<_, _> = self.attributes.range((at, 0)..=(at, u16::MAX))
                    .filter(|(_, f)| f.value != Value::Null)
                    .map(|((_, key), f)| (*key, f.value.clone())).collect();
                if let Some(value) = self.attribute(at, attr::CHECKBOX) { attrs.insert(attr::CHECKBOX, value.clone()); }
                blocks.push(Block { at, text: String::new(), attrs });
            } else {
                blocks.last_mut().unwrap().text.push(node.ch());
            }
        }
        blocks
    }
}

fn text_attr<'a>(attrs: &'a BTreeMap<u16, Value>, key: u16) -> &'a str {
    match attrs.get(&key) { Some(Value::Text(text)) => text, _ => "" }
}

pub fn to_markdown(doc: &DocState) -> String { markdown_blocks(&doc.blocks()) }

#[derive(Clone, Copy, PartialEq, Eq)]
enum MarkdownForm { WithTaskLinks, Editor }

/// Markdown shown to a person. Task links stay on their block delimiters.
pub fn editor_markdown(doc: &DocState) -> String { render_blocks(&doc.blocks(), MarkdownForm::Editor) }

impl Block {
    /// Keep indentation separate so a paged export can emit just the requested spaces.
    pub fn markdown_line(&self) -> (usize, String) {
        self.line(MarkdownForm::WithTaskLinks)
    }

    fn line(&self, form: MarkdownForm) -> (usize, String) {
        let mut out = String::new();
        let spaces = match self.attrs.get(&attr::INDENT) {
            Some(Value::Int(n)) if (1..=MAX_INDENT_LEVELS).contains(n) => 2 * *n as usize,
            Some(Value::Int(n)) if *n > MAX_INDENT_LEVELS => { out.push_str(&format!("{INDENT_MARKER}{n}) ")); 0 }
            _ => 0,
        };
        let prefix = match text_attr(&self.attrs, attr::KIND) {
            "heading1" => "# ", "heading2" => "## ", "heading3" => "### ",
            "bullet" => "- ", "numbered" => "1. ",
            "todo" if text_attr(&self.attrs, attr::CHECKBOX) == "done" => "- [x] ",
            "todo" if text_attr(&self.attrs, attr::CHECKBOX) == "review" => "- [~] ",
            "todo" => "- [ ] ", _ => "",
        };
        out.push_str(prefix);
        out.push_str(&self.text);
        if let Some(Value::Id(task)) = self.attrs.get(&attr::TASK).filter(|_| form == MarkdownForm::WithTaskLinks) {
            out.push_str(" (task ");
            out.push_str(&crate::hex(task));
            out.push(')');
        }
        (spaces, out)
    }
}

pub fn markdown_blocks(blocks: &[Block]) -> String { render_blocks(blocks, MarkdownForm::WithTaskLinks) }

fn render_blocks(blocks: &[Block], form: MarkdownForm) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 { out.push('\n'); }
        let (spaces, line) = block.line(form);
        out.extend(std::iter::repeat_n(' ', spaces));
        out.push_str(&line);
    }
    out
}

/// The most nesting levels shown as spaces. No editor or Markdown reader shows more than a
/// handful of levels distinctly, so 16 leaves room above any real outline and bounds the
/// whitespace one line can cost. A deeper block keeps its stored indent and shows
/// `(indent n) ` instead; this is a display rule, never a limit on the data.
const MAX_INDENT_LEVELS: i64 = 16;
const INDENT_MARKER: &str = "(indent ";

fn parse_line(mut line: &str, form: MarkdownForm) -> (&str, BTreeMap<u16, Value>) {
    let mut attrs = BTreeMap::new();
    let levels = line.bytes().take_while(|b| *b == b' ').count() / 2;
    if levels > 0 {
        attrs.insert(attr::INDENT, Value::Int(levels as i64));
        line = &line[2 * levels..];
    } else if let Some((n, rest)) = line.strip_prefix(INDENT_MARKER).and_then(|r| r.split_once(") "))
        .and_then(|(n, rest)| Some((n.parse::<i64>().ok().filter(|n| *n > MAX_INDENT_LEVELS)?, rest))) {
        // A line the display rule wrote: the marker stands for the stored indent.
        attrs.insert(attr::INDENT, Value::Int(n));
        line = rest;
    }
    for (prefix, kind) in [("- [~] ", "todo"), ("- [ ] ", "todo"), ("- [x] ", "todo"), ("### ", "heading3"),
        ("## ", "heading2"), ("# ", "heading1"), ("- ", "bullet"), ("1. ", "numbered")] {
        if let Some(body) = line.strip_prefix(prefix) {
            attrs.insert(attr::KIND, Value::Text(kind.into()));
            if kind == "todo" {
                attrs.insert(attr::CHECKBOX, Value::Text(match prefix { "- [x] " => "done", "- [~] " => "review", _ => "open" }.into()));
            }
            line = body;
            break;
        }
    }
    if form == MarkdownForm::Editor { return (line, attrs) }
    if let Some((body, suffix)) = line.strip_suffix(')').and_then(|s| s.rsplit_once(" (task ")) {
        if let Some(task) = crate::parse_id(suffix).filter(|_| suffix.len() == 64) {
            attrs.insert(attr::TASK, Value::Id(task));
            line = body;
        }
    }
    (line, attrs)
}

// Hirschberg LCS uses two rows of scores. Strip common ends before subdividing so a
// small edit in a long line (or a long document) only scores the changed middle.
fn matches<T: Eq>(a: &[T], b: &[T], mut i: usize, mut j: usize, out: &mut Vec<(usize, usize)>) {
    let prefix = a.iter().zip(b).take_while(|(a, b)| a == b).count();
    out.extend((0..prefix).map(|n| (i + n, j + n)));
    let (a, b) = (&a[prefix..], &b[prefix..]);
    i += prefix;
    j += prefix;
    let suffix = a.iter().rev().zip(b.iter().rev()).take_while(|(a, b)| a == b).count();
    let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);
    if a.len() == 1 {
        if let Some(n) = b.iter().position(|c| c == &a[0]) { out.push((i, j + n)); }
    } else if !a.is_empty() && !b.is_empty() {
        fn scores<T: Eq>(a: &[T], b: &[T], reverse: bool) -> Vec<usize> {
            let mut row = vec![0; b.len() + 1];
            for m in 0..a.len() {
                let left = &a[if reverse { a.len() - 1 - m } else { m }];
                let mut diagonal = 0;
                for n in 0..b.len() {
                    let above = row[n + 1];
                    let right = &b[if reverse { b.len() - 1 - n } else { n }];
                    row[n + 1] = if left == right { diagonal + 1 } else { above.max(row[n]) };
                    diagonal = above;
                }
            }
            row
        }
        let mid = a.len() / 2;
        let split = {
            let front = scores(&a[..mid], b, false);
            let back = scores(&a[mid..], b, true);
            (0..=b.len()).max_by_key(|&k| front[k] + back[b.len() - k]).unwrap()
        };
        matches(&a[..mid], &b[..split], i, j, out);
        matches(&a[mid..], &b[split..], i + mid, j + split, out);
    }
    out.extend((0..suffix).map(|n| (i + a.len() + n, j + b.len() + n)));
}

pub fn plan_write(doc: &DocState, markdown: &str) -> Vec<TextOp> {
    plan_write_nodes(doc, &doc.nodes, &doc.blocks(), Anchor::Root, markdown, MarkdownForm::WithTaskLinks)
}

/// Diff visible Markdown without interpreting or updating hidden task links.
pub fn plan_editor_write(doc: &DocState, markdown: &str) -> Vec<TextOp> {
    plan_write_nodes(doc, &doc.nodes, &doc.blocks(), Anchor::Root, markdown, MarkdownForm::Editor)
}

/// Diff only the previously read blocks, provided this exact range is still live and unchanged.
pub fn plan_write_range(doc: &DocState, seen: &[Block], markdown: &str) -> Option<Vec<TextOp>> {
    let first = doc.position(seen.first()?.at)?;
    let last = doc.position(seen.last()?.at)?;
    let live_break = |i: usize| doc.nodes[i].ch() == '\n' && (i == 0 || doc.nodes[i].live());
    if first > last || !live_break(first) || !live_break(last) { return None }
    let nodes = &doc.nodes[first..doc.end_of_block(last)];
    if doc.blocks_in(nodes) != seen { return None }
    // Root as an insertion anchor means the physical beginning, even if the original
    // root block moved. Range inserts follow the selected block's actual delimiter.
    let anchor = Anchor::Char(doc.id(doc.nodes[first]));
    Some(plan_write_nodes(doc, nodes, seen, anchor, markdown, MarkdownForm::WithTaskLinks))
}

fn plan_write_nodes(doc: &DocState, nodes: &[Node], old_blocks: &[Block], first_anchor: Anchor, markdown: &str, form: MarkdownForm) -> Vec<TextOp> {
    let old_lines: Vec<_> = old_blocks.iter().map(|b| b.text.as_str()).collect();
    let parsed: Vec<_> = markdown.split('\n').map(|line| parse_line(line, form)).collect();
    let new_lines: Vec<_> = parsed.iter().map(|(text, _)| *text).collect();
    let mut same_lines = Vec::new();
    matches(&old_lines, &new_lines, 0, 0, &mut same_lines);

    fn flatten(lines: &[&str]) -> (Vec<char>, Vec<usize>) {
        let mut chars = Vec::new();
        let mut starts = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if index != 0 { chars.push('\n'); }
            starts.push(chars.len());
            chars.extend(line.chars());
        }
        (chars, starts)
    }
    let (old, old_starts) = flatten(&old_lines);
    let (new, new_starts) = flatten(&new_lines);
    let live: Vec<_> = nodes[1..].iter().filter(|n| n.live()).collect();
    let id = |i: usize| doc.id(*live[i]);
    let mut same = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut paired = Vec::new();
    let (mut old_line, mut new_line) = (0, 0);
    for (a, b) in same_lines.into_iter().chain([(old_lines.len(), new_lines.len())]) {
        // Changed lines pair in order between exact matches. A split/merge leaves the
        // unmatched tail together so its surviving characters can cross the edited break.
        for (x, y) in (old_line..a).zip(new_line..b) {
            if x > 0 && y > 0 { paired.push((old_starts[x] - 1, new_starts[y] - 1, 1)); }
        }
        if a == old_lines.len() { break }
        let delimiter = usize::from(a > 0 && b > 0);
        let x = old_starts[a] - delimiter;
        let y = new_starts[b] - delimiter;
        paired.push((x, y, old_lines[a].chars().count() + delimiter));
        (old_line, new_line) = (a + 1, b + 1);
    }
    for (x, y, count) in paired {
        matches(&old[i..x], &new[j..y], i, j, &mut same);
        same.extend((0..count).map(|n| (x + n, y + n)));
        (i, j) = (x + count, y + count);
    }
    matches(&old[i..], &new[j..], i, j, &mut same);
    same.push((old.len(), new.len()));

    let mut ops = Vec::new();
    let mut breaks = vec![doc.marker(nodes[0])];
    let mut inserted = 0usize;
    (i, j) = (0, 0);
    for (x, y) in same {
        if i < x { ops.push(TextOp::Delete { from: id(i), to: id(x - 1) }); }
        if j < y {
            let anchor = if i == 0 { first_anchor } else { Anchor::Char(id(i - 1)) };
            ops.push(TextOp::Insert { anchor, text: new[j..y].iter().collect() });
            for (offset, ch) in new[j..y].iter().enumerate() {
                if *ch == '\n' { breaks.push(BreakRef::Inserted((inserted + offset) as u32)); }
            }
            inserted += y - j;
        }
        if new.get(y) == Some(&'\n') {
            breaks.push(if id(x) == doc.root_id() { BreakRef::Root } else { BreakRef::Char(id(x)) });
        }
        (i, j) = (x + 1, y + 1);
    }
    for (at, (_, mut attrs)) in breaks.into_iter().zip(parsed) {
        // Compare against the actual link on the surviving delimiter. This also
        // routes an edited checkbox through the same task-state logic as a tool tick.
        if form == MarkdownForm::Editor {
            if let Some(link) = doc.attribute(at, attr::TASK) { attrs.insert(attr::TASK, link.clone()); }
        }
        for key in [attr::KIND, attr::INDENT, attr::TASK, attr::CHECKBOX] {
            if key == attr::CHECKBOX && doc.attribute(at, attr::TASK).is_some() && !attrs.contains_key(&attr::TASK) {
                // Unlink takes the checkbox from the task at the signed event's replay position.
                continue
            }
            if key == attr::CHECKBOX && attrs.contains_key(&attr::TASK) && !attrs.contains_key(&key) { continue }
            if doc.attribute(at, key) != attrs.get(&key) {
                ops.push(TextOp::Attr { at, attr: key, value: attrs.get(&key).cloned().unwrap_or(Value::Null) });
            }
        }
    }
    ops
}

use crate::fold::cache::{fields as cache_fields, Cached};
cache_fields!(Node { run, index, scalar });
cache_fields!(Run { event, positions });
cache_fields!(DocState { nodes, runs, slots, overlaps, attributes, inverses, undone, task_states, checkbox_sources });

impl Cached for BreakRef {
    fn save(&self, out: &mut Vec<u8>) { self.bytes().save(out); }
    fn load(input: &mut &[u8]) -> Option<Self> { Self::decode(&Vec::<u8>::load(input)?) }
}

impl Cached for CopiedStatus {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            Self::TaskStatus(writer) => { out.push(0); writer.save(out); }
            Self::SameEventTick { at, link_writer } => { out.push(1); at.save(out); link_writer.save(out); }
        }
    }
    fn load(input: &mut &[u8]) -> Option<Self> {
        Some(match u8::load(input)? {
            0 => Self::TaskStatus(Cached::load(input)?),
            1 => Self::SameEventTick { at: Cached::load(input)?, link_writer: Cached::load(input)? },
            _ => return None,
        })
    }
}
cache_fields!(CheckboxSource { task, link_writer, status });

impl Cached for Inverse {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            Self::Visibility { run, first, last, hide } => {
                out.push(u8::from(*hide)); run.save(out); first.save(out); last.save(out);
            }
            Self::Attr { at, attr, before, before_writer, before_origin, before_source } => {
                out.push(2); at.save(out); attr.save(out); before.save(out); before_writer.save(out); before_origin.save(out); before_source.save(out);
            }
            Self::Move { block, before, stop } => { out.push(3); block.save(out); before.save(out); stop.save(out); }
        }
    }
    fn load(input: &mut &[u8]) -> Option<Self> {
        Some(match u8::load(input)? {
            tag @ 0..=1 => Self::Visibility { run: usize::load(input)?, first: u32::load(input)?, last: u32::load(input)?, hide: tag == 1 },
            2 => Self::Attr { at: Cached::load(input)?, attr: Cached::load(input)?, before: Cached::load(input)?,
                before_writer: Cached::load(input)?, before_origin: Cached::load(input)?, before_source: Cached::load(input)? },
            3 => Self::Move { block: Cached::load(input)?, before: Cached::load(input)?, stop: Cached::load(input)? },
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Content, FieldWriter};

    #[test]
    fn markdown_writes_preserve_unchanged_lines_and_characters() {
        let mut doc = DocState::new([1; 32]);
        let mixed = "# Header\n  ## Sub\n### Three\n- item\n1. number\n- [ ] tick\n- [x] done\n\nstray ** 🦀\n";
        doc.apply([2; 32], &plan_write(&doc, mixed), |_| true);
        assert_eq!(to_markdown(&doc), mixed, "markup, indent, empty lines and stray punctuation round trip");
        assert!(plan_write(&doc, mixed).is_empty(), "an unchanged document writes nothing");
        let ticked = mixed.replace("[ ]", "[x]");
        let ops = plan_write(&doc, &ticked);
        assert!(matches!(ops.as_slice(), [TextOp::Attr { attr: attr::CHECKBOX, .. }]), "ticking changes exactly one checkbox register");
        doc.apply([3; 32], &ops, |_| true);
        let large = (0..200).map(|n| format!("- line {n} red")).collect::<Vec<_>>().join("\n");
        doc.apply([4; 32], &plan_write(&doc, &large), |_| true);
        let before = doc.node_count();
        let edited = large.replace("100 red", "100 cyan");
        let ops = plan_write(&doc, &edited);
        assert_eq!(ops.iter().filter(|op| matches!(op, TextOp::Insert { .. })).count(), 1, "one word in 200 lines produces one run");
        doc.apply([5; 32], &ops, |_| true);
        assert_eq!(doc.node_count() - before, 4, "only the four new scalars consume nodes");
        assert_eq!(to_markdown(&doc), edited, "the small edit produces the requested markdown");
        for (i, next) in ["", "\n", "\n# x\n\n", "first\n\n# x", "# x", "x\nx\n", "\nx\nx", "x"].iter().enumerate() {
            doc.apply([10 + i as u8; 32], &plan_write(&doc, next), |_| true);
            assert_eq!(to_markdown(&doc), *next, "duplicate, leading and trailing empty lines survive rewrite {i}");
        }
    }

    #[test]
    fn indent_beyond_the_display_depth_shows_a_marker_and_keeps_the_stored_value() {
        let mut doc = DocState::new([1; 32]);
        doc.apply([2; 32], &plan_write(&doc, "- one\n- two\n- three"), |_| true);
        let breaks: Vec<_> = doc.blocks().iter().map(|b| b.at).collect();
        let set = |doc: &mut DocState, n, at: BreakRef, level: i64| {
            doc.apply([n; 32], &[TextOp::Attr { at, attr: attr::INDENT, value: Value::Int(level) }], |_| true)
        };
        set(&mut doc, 3, breaks[0], i64::MAX);
        set(&mut doc, 4, breaks[1], MAX_INDENT_LEVELS);
        set(&mut doc, 5, breaks[2], MAX_INDENT_LEVELS + 1);
        let markdown = to_markdown(&doc);
        let lines: Vec<_> = markdown.split('\n').collect();
        assert_eq!(lines[0], format!("(indent {}) - one", i64::MAX), "a huge indent renders the marker");
        assert_eq!(lines[1], format!("{}- two", " ".repeat(32)), "an indent at the cap renders spaces");
        assert_eq!(lines[2], "(indent 17) - three", "one past the cap renders the marker");
        assert!(markdown.len() < 100, "the rendered size stays small");
        let stored = |doc: &DocState| doc.blocks().iter().map(|b| b.attrs.get(&attr::INDENT).cloned()).collect::<Vec<_>>();
        let before = stored(&doc);
        assert_eq!(before[0], Some(Value::Int(i64::MAX)), "the stored value is exact");
        assert!(plan_write(&doc, &markdown).is_empty(), "an unchanged document, markers included, writes nothing");
        assert!(plan_editor_write(&doc, &markdown).is_empty(), "the editor form keeps the marker too");
        let edited = markdown.replace("- one", "- uno").replace("- three", "- tres");
        doc.apply([6; 32], &plan_write(&doc, &edited), |_| true);
        assert_eq!(stored(&doc), before, "editing deep lines keeps their stored indent");
        assert_eq!(doc.blocks()[0].text, "uno", "the edit lands in the text, not the marker");
        assert_eq!(to_markdown(&doc), edited, "the marker is not turned into text");
    }

    #[test]
    fn a_delete_spares_text_typed_concurrently_inside_its_range() {
        let base = [2; 32];
        let id = |index| CharId { event: base, index };
        let mut state = DocState::new([1; 32]);
        state.apply(base, &[TextOp::Insert { anchor: Anchor::Root, text: "abc".into() }], |_| true);
        // Ordered before the delete but not in its past: typed while the other person deleted.
        state.apply([3; 32], &[TextOp::Insert { anchor: Anchor::Char(id(0)), text: "X".into() }], |_| true);
        state.apply([4; 32], &[TextOp::Delete { from: id(0), to: id(2) }], |event| event == base);
        assert_eq!(state.text(), "X", "the concurrent insert survives a delete of the text around it");
        state.undo([5; 32], [4; 32]);
        assert_eq!(state.text(), "aXbc", "undo restores exactly what the delete hid");
    }

    #[test]
    fn bursts_round_trip_and_move_their_ids_and_attributes() {
        let doc = [1; 32];
        let first = [2; 32];
        let id = |index| CharId { event: first, index };
        let initial = TextOp::Insert { anchor: Anchor::Root, text: "A\nβx\nC".into() };
        let mut state = DocState::new(doc);
        state.apply(first, &[initial], |_| true);
        let ops = vec![
            TextOp::Insert { anchor: Anchor::Char(id(2)), text: "!".into() },
            TextOp::Delete { from: id(3), to: id(3) },
            TextOp::Attr { at: BreakRef::Char(id(1)), attr: attr::INDENT, value: Value::Int(2) },
            TextOp::Move { block: BreakRef::Char(id(1)), after: BreakRef::Char(id(4)) },
        ];
        let content = Content::Text { doc, ops: ops.clone() };
        let mut wire = content.encode();
        let after_insert: usize = crate::event::fields(&wire).unwrap().iter().take(2).map(|(_, bytes)| 5 + bytes.len()).sum();
        wire.splice(after_insert..after_insert, FieldWriter::default().bytes(99, b"future op").finish());
        assert_eq!(Content::decode(8, 1, &wire), Some(content), "an unknown envelope preserves both neighbouring operations");
        state.apply([3; 32], &ops, |_| true);
        assert_eq!(state.text(), "A\nC\nβ!", "the whole edited block moves");
        assert_eq!(state.attribute(BreakRef::Char(id(1)), attr::INDENT), Some(&Value::Int(2)), "attributes follow the preceding delimiter");
        state.undo([4; 32], [3; 32]);
        assert_eq!(state.text(), "A\nβx\nC", "undo restores the block, deletion and insertion together");
        state.apply([5; 32], &[TextOp::Move { block: BreakRef::Root, after: BreakRef::Char(id(4)) }], |_| true);
        assert_eq!(state.text(), "βx\nC\nA", "the original first block also moves with its own identity");
        let mut range = state.clone();
        let before = range.blocks();
        let ops = plan_write_range(&range, &before[2..], "## Ω\nextra").unwrap();
        range.apply([60; 32], &ops, |_| true);
        assert_eq!(&range.blocks()[..2], &before[..2], "writing the moved root leaves both preceding blocks intact");
        assert_eq!(markdown_blocks(&range.blocks()[2..]), "## Ω\nextra", "the replacement starts at the moved root");
        state.undo([6; 32], [5; 32]);
        assert_eq!(state.text(), "A\nβx\nC", "undo can restore a block to the very start");
        let mut merged = state.clone();
        merged.apply([7; 32], &[TextOp::Move { block: BreakRef::Char(id(4)), after: BreakRef::Root }], |_| true);
        merged.apply([8; 32], &[TextOp::Delete { from: id(1), to: id(1) }], |_| true);
        merged.undo([9; 32], [7; 32]);
        assert_eq!(merged.text(), "Aβx\nC", "undo move preserves a later deletion of the delimiter beside the moved text");
        merged.undo([10; 32], [9; 32]);
        assert_eq!(merged.text(), "A\nCβx", "redo uses the original gap even when its break is now hidden");
        state.apply([7; 32], &[TextOp::Insert { anchor: Anchor::Char(id(5)), text: "\nD".into() }], |_| true);
        let last_block = BreakRef::Char(CharId { event: [7; 32], index: 0 });
        state.apply([8; 32], &[TextOp::Move { block: BreakRef::Char(id(4)), after: last_block }], |_| true);
        state.apply([9; 32], &[TextOp::Delete { from: id(1), to: id(1) }], |_| true);
        state.undo([90; 32], [8; 32]);
        assert_eq!(state.text(), "Aβx\nC\nD", "a move can return behind its original predecessor after that break was tombstoned");
        let mut seed = 9u32;
        let mut markdown = String::new();
        for generation in 0..60 {
            markdown.clear();
            for _ in 0..12 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                markdown.push(['a', 'b', '\n', 'β'][(seed >> 16) as usize % 4]);
            }
            state.apply([10 + generation; 32], &plan_write(&state, &markdown), |_| true);
            assert_eq!(to_markdown(&state), markdown, "diff generation {generation} preserves all requested scalars");
        }
    }
}
