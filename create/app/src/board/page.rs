//! Canonical Splash projection of a folded board. Positions are integers until written.
use super::schema::{Field, Literal, Source, Vocabulary};
use super::tree::{live, BoardTree};
use makepad_work_core::entity::{field, Entity};
use makepad_work_core::event::{Anchor, Id, Key, Value};
use makepad_work_core::fold::State;
use makepad_work_core::{blob, hex};
use makepad_network::digest::Sha256;
use std::collections::HashMap;
use std::fmt::Write;

mod paging;
pub use paging::{read, Cursor};

fn coord(item: &Entity, key: Key) -> i64 {
    match item.value(key) { Some(Value::Int(n)) => *n, _ => 0 }
}

fn units(out: &mut String, value: i64) {
    if value < 0 { out.push('-'); }
    let n = value.unsigned_abs();
    write!(out, "{}", n / 64).unwrap();
    if n % 64 != 0 {
        let fraction = format!("{:06}", (n % 64) * 15625);
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
}

fn string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"), '\r' => out.push_str("\\r"), '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{{{:x}}}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn reference(out: &mut String, id: &Id) { out.push('@'); out.push_str(&hex(id)); }

fn literal(out: &mut String, field: &Field, item: &Entity, state: &State) -> bool {
    let key = match &field.source {
        Source::Register(key) => key,
        Source::Identity => { reference(out, &item.id); return true }
        Source::Children => { out.push('['); return true }
        Source::RootTitle => { string(out, item.text(field::TITLE)); return true }
        Source::Body => {
            if let Some(doc) = state.docs.get(&item.id) { string(out, &doc.text()); }
            else if let Some(Value::Text(s)) = item.value(field::TEXT) { string(out, s); }
            else { return false }
            return true
        }
    };
    match (field.form, item.value(key.clone())) {
        (Literal::Units, _) => units(out, coord(item, key.clone())),
        (Literal::Text, Some(Value::Text(s))) => string(out, s),
        (Literal::Color, Some(Value::Int(n))) => {
            let Ok(n) = u32::try_from(*n) else { return false };
            if n <= 0xffffff { write!(out, "#{n:06x}").unwrap(); }
            else { write!(out, "#{n:08x}").unwrap(); }
        }
        (Literal::Enum, Some(Value::Text(s))) => {
            // Free-form strings could be written by slice 1. Quote a non-name losslessly.
            if s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                out.push('@'); out.push_str(s);
            } else { string(out, s); }
        }
        (Literal::Bool, Some(Value::Bool(b))) => out.push_str(if *b { "true" } else { "false" }),
        (Literal::Reference, Some(Value::Id(id))) => reference(out, id),
        (Literal::Reference, Some(Value::Blob { size, root })) => reference(out, &blob::id(*size, root)),
        (Literal::Anchor, Some(Value::Anchor(anchor))) => match anchor {
            Anchor::Spot { x, y } => {
                out.push_str("Spot{x: "); units(out, *x); out.push_str(" y: "); units(out, *y); out.push('}');
            }
            Anchor::Time { ms } => write!(out, "Time{{ms: {ms}}}").unwrap(),
            Anchor::Text { run, index } => {
                out.push_str("Text{run: "); reference(out, run); write!(out, " index: {index}}}").unwrap();
            }
        },
        _ => return false,
    }
    true
}

fn close_constructor(out: &mut String, entity: &Entity, properties: bool) {
    if properties {
        for (key, field) in entity.open_fields() {
            let Value::Literal(src) = &field.value else { continue };
            write!(out, " {key}: {src}").unwrap();
            // Separate the following token if the literal could end in a line comment.
            if src.rsplit('\n').next().is_some_and(|line| line.contains("//")) { out.push('\n'); }
        }
    }
    out.push('}');
}

fn constructor<'a>(out: &mut String, name: &str, fields: impl Iterator<Item = &'a Field>,
                   entity: &Entity, state: &State, title: bool) -> bool {
    out.push_str(name);
    out.push('{');
    for field in fields {
        if !title && matches!(field.source, Source::RootTitle) { continue }
        let before = out.len();
        if !out.ends_with('{') { out.push(' '); }
        out.push_str(field.name);
        out.push_str(": ");
        if !literal(out, field, entity, state) {
            out.truncate(before);
        }
        if matches!(field.form, Literal::Children) { return true }
    }
    close_constructor(out, entity, true);
    false
}

fn intersects(e: &Entity, [x0, y0, x1, y1]: [f64; 4]) -> bool {
    let axis = |a, size, low, high| {
        let a = i128::from(coord(e, a));
        let b = a + i128::from(coord(e, size));
        (a.min(b) as f64 / 64.0) <= high && (a.max(b) as f64 / 64.0) >= low
    };
    axis(field::X, field::W, x0, x1) && axis(field::Y, field::H, y0, y1)
}

struct Node<'a> {
    entity: &'a Entity,
    parent: Option<usize>,
    depth: usize,
    end: usize, // exclusive end of its projected subtree
}

struct Board<'a> {
    state: &'a State,
    root: &'a Entity,
    nodes: Vec<Node<'a>>,
    // Includes nonintersecting items so a cursor moved out of the rectangle can resume.
    after: HashMap<Id, usize>,
    revision: String,
}

impl<'a> Board<'a> {
    fn new(state: &'a State, id: Id, rect: Option<[f64; 4]>) -> Result<Self, String> {
        let schema = Vocabulary::Board.schema();
        let root = state.entities.get(&id).filter(|e| e.kind.number() == schema.root.entity_kind && live(e))
            .ok_or_else(|| format!("unknown, deleted or archived board {}", hex(&id)))?;
        let mut tree = BoardTree::new(state);
        let mut children: HashMap<Id, Vec<&Entity>> = HashMap::new();
        for item in state.entities.values().filter(|e| live(e) && super::tree::board_kind(&e.kind)) {
            if let Some(p) = tree.placement(item).filter(|p| p.board == id) {
                children.entry(p.section.unwrap_or(id)).or_default().push(item);
            }
        }
        for siblings in children.values_mut() {
            siblings.sort_unstable_by_key(|e| (e.z_key(), e.id));
        }

        // Hash identities, effective parentage and field writers in painter order. The board
        // revision is independent of the rectangle and needs no formatting of off-page bodies.
        let mut hash = Sha256::new();
        let mut stamp = |e: &Entity, parent: Id| {
            hash.update(&e.id); hash.update(&parent);
            hash.update(&(e.fields.len() as u64).to_le_bytes());
            for (key, f) in &e.fields {
                match key {
                    Key::Number(n) => { hash.update(&[0]); hash.update(&n.to_le_bytes()); }
                    Key::Name(name) => {
                        hash.update(&[1]); hash.update(&(name.len() as u64).to_le_bytes()); hash.update(name.as_bytes());
                    }
                }
                hash.update(&f.writer);
            }
            if let Some(rev) = state.doc_revision(e.id) { hash.update(rev.as_bytes()); }
        };
        stamp(root, id);
        let mut nodes: Vec<Node> = Vec::new();
        let mut after = HashMap::new();
        // Each frame owns one sibling iterator. Depth costs heap space, never call-stack space.
        let mut stack = vec![(id, None, None::<usize>, children.remove(&id).unwrap_or_default().into_iter())];
        while let Some((owner, parent, close, siblings)) = stack.last_mut() {
            let Some(e) = siblings.next() else {
                if let Some(i) = *close { nodes[i].end = nodes.len(); }
                stack.pop();
                continue
            };
            stamp(e, *owner);
            let mut child_parent = *parent;
            let mut close = None;
            if rect.is_none_or(|r| intersects(e, r)) {
                let depth = parent.map_or(0, |i: usize| nodes[i].depth + 1);
                let i = nodes.len();
                nodes.push(Node { entity: e, parent: *parent, depth, end: i + 1 });
                child_parent = Some(i);
                close = Some(i);
            }
            after.insert(e.id, nodes.len());
            if let Some(kids) = children.remove(&e.id) {
                stack.push((e.id, child_parent, close, kids.into_iter()));
            }
        }
        Ok(Self { state, root, nodes, after, revision: hex(&hash.finalise()) })
    }

    fn header(&self, title: bool) -> String {
        let mut out = String::new();
        let root = &Vocabulary::Board.schema().root;
        constructor(&mut out, root.name, root.fields.iter(), self.root, self.state, title);
        out
    }

    fn footer(&self, properties: bool) -> String {
        let mut out = String::from("]");
        close_constructor(&mut out, self.root, properties);
        out
    }

    fn append(&self, out: &mut String, open: &mut Vec<usize>, i: usize, base_depth: usize) {
        let node = &self.nodes[i];
        self.finish(out, open, node.depth - base_depth);
        if !out.is_empty() && !out.ends_with('[') { out.push(' '); }
        let schema = Vocabulary::Board.schema();
        let (name, extra) = match &node.entity.kind {
            Key::Number(id) => {
                let ctor = schema.item(*id).unwrap();
                (ctor.name, ctor.fields)
            }
            Key::Name(name) => (name.as_str(), &[][..]),
        };
        if constructor(out, name, schema.item_fields.iter().chain(extra), node.entity, self.state, true) {
            open.push(i);
        }
    }

    fn finish(&self, out: &mut String, open: &mut Vec<usize>, depth: usize) {
        while open.len() > depth {
            let node = &self.nodes[open.pop().unwrap()];
            out.push(']');
            close_constructor(out, node.entity, true);
        }
    }

    fn subtree(&self, i: usize) -> String {
        let mut out = String::new();
        let mut open = Vec::new();
        for n in i..self.nodes[i].end { self.append(&mut out, &mut open, n, self.nodes[i].depth); }
        self.finish(&mut out, &mut open, 0);
        out
    }
}

/// Print one entire board, independent of tool pagination.
pub fn print(state: &State, board: Id) -> Result<String, String> {
    let board = Board::new(state, board, None)?;
    let mut out = board.header(true);
    let mut open = Vec::new();
    for i in 0..board.nodes.len() { board.append(&mut out, &mut open, i, 0); }
    board.finish(&mut out, &mut open, 0);
    out.push_str(&board.footer(true));
    Ok(out)
}
