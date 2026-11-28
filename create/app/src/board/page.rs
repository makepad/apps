//! Canonical Splash projection of a folded board. Positions are integers until written.
use super::schema::{Field, Literal, Source, Vocabulary};
use super::tree::{live, BoardTree};
use crate::ai_out::SafeToolError;
use makepad_town_core::trust::{AiOut, AiTextBuilder, ForAi, Gate, Withheld};
use makepad_town_core::entity::{field, kind, Entity};
use makepad_town_core::event::{Anchor, Id, Key};
use makepad_town_core::fold::State;
use makepad_town_core::{blob, hex};
use makepad_network::digest::Sha256;
use std::collections::HashMap;

mod paging;
pub use paging::{read, Cursor};

fn withheld(why: Withheld) -> String { SafeToolError::withheld(why).as_str().to_owned() }

fn check_state(state: &State, gate: &Gate<'_>) -> Result<(), String> {
    if !gate.is_bound_to(state) { return Err(withheld(Withheld::MissingSource)) }
    gate.town_readable().map_err(withheld)
}

fn coord(gate: &Gate<'_>, id: &Id, key: &Key) -> Result<i64, Withheld> {
    match gate.entity_field_value(id, key) {
        Ok(v) => Ok(v.as_i64().unwrap_or(0)),
        Err(Withheld::MissingSource) => Ok(0),
        Err(why) => Err(why),
    }
}

fn units(out: &mut AiTextBuilder, value: i64) {
    if value < 0 { out.push_static("-"); }
    let n = value.unsigned_abs();
    out.push_u64(n / 64);
    let mut fraction = (n % 64) * 15625;
    if fraction != 0 {
        out.push_static(".");
        let mut div = 100_000;
        while fraction != 0 {
            out.push_u64(fraction / div);
            fraction %= div;
            div /= 10;
        }
    }
}

fn reference(out: &mut AiTextBuilder, id: &Id) { out.push_static("@"); out.push_id(id); }

fn literal(out: &mut AiTextBuilder, field: &Field, item: &Entity, state: &State, gate: &Gate<'_>) -> Option<()> {
    let key = match &field.source {
        Source::Register(key) => key,
        Source::Writer(key) => return Some(reference(out, &gate.entity_field_writer(&item.id, key).ok()?)),
        Source::DesignPreset => {
            let w = coord(gate, &item.id, &field::W).ok()?;
            let h = coord(gate, &item.id, &field::H).ok()?;
            out.push_static("@");
            return Some(out.push_static(super::design::preset_for((w, h))))
        }
        Source::Identity => return Some(reference(out, &item.id)),
        Source::Children => return Some(out.push_static("[")),
        Source::RootTitle => return match gate.entity_field_text(&item.id, &field::TITLE) {
            Ok(t) => Some(out.push_splash_quoted(t.as_for_ai_str())),
            Err(Withheld::MissingSource) => Some(out.push_static("\"\"")),
            Err(_) => None,
        },
        Source::Body => {
            let text = if state.docs.contains_key(&item.id) { gate.doc_full_text(&item.id) }
                else { gate.entity_field_text(&item.id, &field::TEXT) }.ok()?;
            return Some(out.push_splash_quoted(text.as_for_ai_str()))
        }
    };
    if matches!(field.form, Literal::Units) { return Some(units(out, coord(gate, &item.id, key).ok()?)) }
    let value = gate.entity_field_value(&item.id, key).ok()?;
    match field.form {
        Literal::Bytes => {
            let bytes = value.as_key_bytes()?;
            out.push_static("\""); out.push_hex_bytes(bytes); out.push_static("\"");
        }
        Literal::Text => out.push_splash_quoted(value.as_text()?),
        Literal::Int => out.push_i64(value.as_i64()?),
        Literal::Color => out.push_splash_color(u32::try_from(value.as_i64()?).ok()?),
        Literal::Enum => out.push_splash_enum(value.as_text()?),
        Literal::Bool => out.push_static(if value.as_bool()? { "true" } else { "false" }),
        Literal::Reference => {
            let id = value.as_id().or_else(|| value.as_blob().map(|(size, root)| blob::id(size, &root)))?;
            reference(out, &id);
        }
        Literal::Anchor => match value.as_anchor()? {
            Anchor::Spot { x, y } => {
                out.push_static("Spot{x: "); units(out, *x); out.push_static(" y: "); units(out, *y); out.push_static("}");
            }
            Anchor::Time { ms } => {
                out.push_static("Time{ms: "); out.push_u64(*ms); out.push_static("}");
            }
            Anchor::Text { run, index } => {
                out.push_static("Text{run: "); reference(out, run); out.push_static(" index: "); out.push_u64((*index).into()); out.push_static("}");
            }
        },
        Literal::Units | Literal::Children => return None,
    }
    Some(())
}

fn open_fields<'a>(gate: &Gate<'a>, entity: &Entity) -> Vec<(ForAi<String>, ForAi<&'a str>)> {
    entity.fields.keys().filter(|k| matches!(k, Key::Name(_))).filter_map(|key| {
        let name = gate.open_key_name(&entity.id, key).ok()?;
        let literal = gate.entity_field_value(&entity.id, key).ok()?.as_literal()?;
        Some((name, literal))
    }).collect()
}

fn close_constructor(out: &mut AiTextBuilder, entity: &Entity, gate: &Gate<'_>, properties: bool) {
    if properties {
        for (name, literal) in open_fields(gate, entity) { out.push_splash_open_field(name.as_for_ai_str(), literal); }
    }
    out.push_static("}");
}

fn constructor<'a>(out: &mut AiTextBuilder, fields: impl Iterator<Item = &'a Field>,
                   entity: &Entity, state: &State, gate: &Gate<'_>, title: bool) -> bool {
    out.push_static("{");
    for field in fields {
        if !title && matches!(field.source, Source::RootTitle) { continue }
        let before = out.len();
        if !out.ends_with_char('{') { out.push_static(" "); }
        out.push_static(field.name);
        out.push_static(": ");
        if literal(out, field, entity, state, gate).is_none() { out.truncate(before); }
        if matches!(field.form, Literal::Children) { return true }
    }
    close_constructor(out, entity, gate, true);
    false
}

fn intersects(gate: &Gate<'_>, e: &Entity, [x0, y0, x1, y1]: [f64; 4]) -> bool {
    let axis = |a, size, low, high| {
        let a = i128::from(coord(gate, &e.id, &a).unwrap_or(0));
        let b = a + i128::from(coord(gate, &e.id, &size).unwrap_or(0));
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

struct Board<'a, 'g> {
    gate: &'g Gate<'a>,
    state: &'a State,
    root: &'a Entity,
    nodes: Vec<Node<'a>>,
    // The presenting path, printed after the items on the first page.
    steps: Vec<&'a Entity>,
    // Includes nonintersecting items so a cursor moved out of the rectangle can resume.
    after: HashMap<Id, usize>,
    revision: Id,
}

impl<'a, 'g> Board<'a, 'g> {
    fn new(state: &'a State, gate: &'g Gate<'a>, id: Id, rect: Option<[f64; 4]>) -> Result<Self, String> {
        check_state(state, gate)?;
        let schema = Vocabulary::Board.schema();
        let root = state.entities.get(&id).filter(|e| e.kind.number() == schema.root.entity_kind && live(e))
            .ok_or_else(|| format!("unknown, deleted or archived board {}", hex(&id)))?;
        gate.entity_visible(&id).map_err(withheld)?;
        let mut tree = BoardTree::new(state);
        let mut children: HashMap<Id, Vec<&Entity>> = HashMap::new();
        for item in state.entities.values().filter(|e| live(e) && super::tree::board_kind(&e.kind)) {
            if let Some(p) = tree.placement(item).filter(|p| p.board == id) {
                if gate.board_item_placement(&item.id, &id).is_err() { continue }
                if item.kind == kind::IMAGE && matches!(gate.entity_field_value(&item.id, &field::BLOB),
                    Err(why) if why != Withheld::MissingSource) { continue }
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
            let fields: Vec<_> = e.fields.keys().filter_map(|key| {
                if matches!(key, Key::Name(_)) { gate.open_key_name(&e.id, key).ok()?; }
                Some((key, gate.entity_field_writer(&e.id, key).ok()?))
            }).collect();
            hash.update(&(fields.len() as u64).to_le_bytes());
            for (key, writer) in fields {
                match key {
                    Key::Number(n) => { hash.update(&[0]); hash.update(&n.to_le_bytes()); }
                    Key::Name(name) => {
                        hash.update(&[1]); hash.update(&(name.len() as u64).to_le_bytes()); hash.update(name.as_bytes());
                    }
                }
                hash.update(&writer);
            }
            if state.docs.contains_key(&e.id) && gate.doc_full_text(&e.id).is_ok() {
                if let Some(rev) = state.doc_revision(e.id) { hash.update(rev.as_bytes()); }
            }
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
            if rect.is_none_or(|r| intersects(gate, e, r)) {
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
        let steps: Vec<_> = super::tree::steps(state, id).into_iter().filter(|e| gate.board_item_placement(&e.id, &id).is_ok()).collect();
        for step in &steps { stamp(step, id); }
        Ok(Self { state, gate, root, nodes, after, steps, revision: hash.finalise() })
    }

    fn header(&self, title: bool) -> AiTextBuilder {
        let mut out = AiTextBuilder::new();
        let root = &Vocabulary::Board.schema().root;
        out.push_static(root.name);
        constructor(&mut out, root.fields.iter(), self.root, self.state, self.gate, title);
        out
    }

    fn footer(&self, properties: bool) -> AiTextBuilder {
        let mut out = AiTextBuilder::new();
        out.push_static("]");
        if properties && !self.steps.is_empty() {
            let step = &Vocabulary::Board.schema().step;
            out.push_static(" "); out.push_static(super::schema::PATH); out.push_static(": [");
            for (i, e) in self.steps.iter().enumerate() {
                if i > 0 { out.push_static(" "); }
                out.push_static(step.name);
                constructor(&mut out, step.fields.iter(), e, self.state, self.gate, true);
            }
            out.push_static("]");
        }
        close_constructor(&mut out, self.root, self.gate, properties);
        out
    }

    /// Whether the root prints more than its title and items: its open properties or a path.
    fn root_extras(&self) -> bool {
        !self.steps.is_empty() || !open_fields(self.gate, self.root).is_empty()
    }

    fn append(&self, out: &mut AiTextBuilder, open: &mut Vec<usize>, i: usize, base_depth: usize) {
        let node = &self.nodes[i];
        self.finish(out, open, node.depth - base_depth);
        if !out.is_empty() && !out.ends_with_char('[') { out.push_static(" "); }
        let schema = Vocabulary::Board.schema();
        let extra = match &node.entity.kind {
            Key::Number(id) => {
                let ctor = schema.item(*id).unwrap();
                out.push_static(ctor.name);
                ctor.fields
            }
            Key::Name(_) => {
                let Ok(name) = self.gate.entity_kind_name(&node.entity.id) else { return };
                out.push_for_ai(name);
                &[][..]
            }
        };
        if constructor(out, schema.item_fields.iter().chain(extra), node.entity, self.state, self.gate, true) {
            open.push(i);
        }
    }

    fn finish(&self, out: &mut AiTextBuilder, open: &mut Vec<usize>, depth: usize) {
        while open.len() > depth {
            let node = &self.nodes[open.pop().unwrap()];
            out.push_static("]");
            close_constructor(out, node.entity, self.gate, true);
        }
    }

    fn subtree(&self, i: usize) -> AiTextBuilder {
        let mut out = AiTextBuilder::new();
        let mut open = Vec::new();
        for n in i..self.nodes[i].end { self.append(&mut out, &mut open, n, self.nodes[i].depth); }
        self.finish(&mut out, &mut open, 0);
        out
    }
}

/// Print one entire board, independent of tool pagination.
pub fn print<'a>(state: &'a State, gate: &Gate<'a>, board: Id) -> Result<AiOut, String> {
    let board = Board::new(state, gate, board, None)?;
    let mut out = board.header(true);
    let mut open = Vec::new();
    for i in 0..board.nodes.len() { board.append(&mut out, &mut open, i, 0); }
    board.finish(&mut out, &mut open, 0);
    out.push_builder(&board.footer(true));
    gate.finish_text(out).map_err(withheld)
}
