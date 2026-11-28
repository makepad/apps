use super::{check_state, withheld, Board};
use makepad_ai_services::wire::MAX_RESULT_BYTES;
use makepad_town_core::entity::field;
use makepad_town_core::event::Id;
use makepad_town_core::fold::State;
use makepad_town_core::parse_id;
use makepad_town_core::trust::{AiJson, AiOut, AiTextBuilder, Gate, Withheld};

const BUDGET: usize = MAX_RESULT_BYTES - 512;
const STALE: &str = "the board changed since that page; read it again from the start";
const STALE_FRAGMENT: &str = "the printed node changed or the fragment cursor is invalid; read again from the start";

#[derive(Debug, PartialEq)]
pub enum Cursor {
    After(Id),
    Fragment { id: Id, hash: Id, index: usize },
}

impl Cursor {
    pub fn parse(s: &str) -> Option<Self> {
        let id = |s: &str| {
            (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| parse_id(s)).flatten()
        };
        let mut parts = s.split('~');
        let anchor = id(parts.next()?)?;
        let Some(digest) = parts.next() else { return Some(Self::After(anchor)) };
        let hash = id(digest)?;
        let index = parts.next()?.parse().ok()?;
        (parts.next().is_none() && index >= 2).then_some(Self::Fragment { id: anchor, hash, index })
    }
}

struct Envelope {
    board: Id,
    revision: Id,
    total: usize,
    parent: Id,
}

impl Envelope {
    fn new(board: &Board<'_, '_>, parent: Id) -> Self {
        Self { board: board.root.id, revision: board.revision, total: board.nodes.len(), parent }
    }

    fn reply(&self, source: AiJson, next: Option<AiJson>, fragment: Option<AiJson>, title: Option<AiJson>) -> AiJson {
        let mut fields = vec![("board", AiJson::id(&self.board)), ("revision", AiJson::id(&self.revision)),
            ("total", AiJson::int(self.total as i64)), ("parent", AiJson::id(&self.parent)), ("splash", source)];
        if let Some(fragment) = fragment { fields.push(("fragment", fragment)); }
        if let Some(title) = title { fields.push(("title", title)); }
        fields.push(("next", next.unwrap_or_else(AiJson::null)));
        AiJson::object(fields)
    }
}

enum Part { Title, Node { at: usize, source: AiTextBuilder } }

impl Board<'_, '_> {
    fn envelope(&self, at: usize) -> Envelope {
        let parent = self.nodes.get(at).and_then(|n| n.parent).map_or(self.root.id, |i| self.nodes[i].entity.id);
        Envelope::new(self, parent)
    }

    fn title_page(&self) -> AiJson {
        let mut title = self.header(true);
        title.push_builder(&self.footer(true));
        self.envelope(0).reply(AiJson::built_str(title), Some(AiJson::id(&self.root.id)), None, None)
    }

    fn fragment(&self, part: Part, cursor: Option<(Id, usize)>, with_title: bool) -> Result<AiJson, String> {
        let (id, source, last, end, kind, env) = match part {
            Part::Title if self.root_extras() => {
                let mut source = self.header(true);
                source.push_builder(&self.footer(true));
                (self.root.id, source, self.root.id, 0, "root", Envelope::new(self, self.root.id))
            }
            Part::Title => {
                let mut source = AiTextBuilder::new();
                match self.gate.entity_field_text(&self.root.id, &field::TITLE) {
                    Ok(t) => source.push_splash_quoted(t.as_for_ai_str()),
                    Err(_) => source.push_static("\"\""),
                }
                (self.root.id, source, self.root.id, 0, "title", Envelope::new(self, self.root.id))
            }
            Part::Node { at, source } => {
                let node = &self.nodes[at];
                (node.entity.id, source, node.entity.id, node.end, "node", self.envelope(at))
            }
        };
        let digest = source.sha256();
        let index = match cursor {
            Some((hash, index)) if hash == digest => index,
            Some(_) => return Err(STALE_FRAGMENT.into()),
            None => 1,
        };
        let continuation = |i| {
            let mut out = AiTextBuilder::new();
            out.push_id(&id); out.push_static("~"); out.push_id(&digest); out.push_static("~"); out.push_u64(i);
            AiJson::built_str(out)
        };
        let metadata = |i, count| AiJson::object(vec![("kind", AiJson::static_str(kind)), ("id", AiJson::id(&id)),
            ("hash", AiJson::id(&digest)), ("index", AiJson::int(i)), ("total", AiJson::int(count))]);

        // Fragment widths must survive changes in total, revision, title and parent.
        let overhead = env.reply(AiJson::static_str(""), Some(continuation(u64::MAX)), Some(metadata(i64::MIN, i64::MIN)), None)
            .encoded_len() + 20 - AiJson::int(self.nodes.len() as i64).encoded_len();
        let bounds = source.json_fragment_bounds(BUDGET - overhead);
        let count = bounds.len() - 1;
        if index == 0 || index > count { return Err(STALE_FRAGMENT.into()) }
        let piece = source.slice_to_json(bounds[index - 1], bounds[index]).ok_or(STALE_FRAGMENT)?;
        let next = if index < count { Some(continuation(index as u64 + 1)) }
            else if end < self.nodes.len() { Some(AiJson::id(&last)) } else { None };
        let title = if with_title {
            match self.gate.entity_field_text(&self.root.id, &field::TITLE) {
                Ok(t) => Some(AiJson::text(t)),
                Err(Withheld::MissingSource) => Some(AiJson::static_str("")),
                Err(_) => None,
            }
        } else { None };
        let reply = env.reply(piece, next, Some(metadata(index as i64, count as i64)), title);
        if with_title && reply.encoded_len() > BUDGET { return Ok(self.title_page()) }
        Ok(reply)
    }

    fn page(&self, after: Option<Cursor>) -> Result<AiJson, String> {
        let first = after.is_none();
        let start = match after {
            None => 0,
            Some(Cursor::After(id)) if id == self.root.id => 0,
            Some(Cursor::After(id)) => {
                let &after = self.after.get(&id).ok_or(STALE)?;
                match after.checked_sub(1).and_then(|i| self.nodes.get(i)) {
                    Some(n) if n.entity.id == id => n.end,
                    _ => after,
                }
            }
            Some(Cursor::Fragment { id, hash, index }) => {
                let part = if id == self.root.id { Part::Title } else {
                    let i = self.after.get(&id).and_then(|n| n.checked_sub(1))
                        .filter(|&i| self.nodes[i].entity.id == id).ok_or(STALE_FRAGMENT)?;
                    Part::Node { at: i, source: self.subtree(i) }
                };
                return self.fragment(part, Some((hash, index)), false)
            }
        };
        let env = self.envelope(start);
        let more = env.reply(AiJson::static_str(""), Some(AiJson::id(&self.root.id)), None, None).encoded_len();
        let done = env.reply(AiJson::static_str(""), None, None, None).encoded_len();
        let mut source = self.header(first);
        let footer = self.footer(first);
        let mut bytes = source.json_encoded_len() + footer.json_encoded_len();
        let total = self.nodes.len();
        if first && bytes + if total > 0 { more } else { done } > BUDGET {
            return self.fragment(Part::Title, None, false)
        }
        let mut at = start;
        let mut last = None;
        if let Some(node) = self.nodes.get(start) {
            // An old id cursor may now be inside a section. Resume there and name the
            // enclosing parent in the envelope. Every constructor includes its whole subtree.
            let limit = node.parent.map_or(total, |p| self.nodes[p].end);
            while at < limit {
                let node = &self.nodes[at];
                let subtree = self.subtree(at);
                let size = subtree.json_encoded_len();
                let overhead = if node.end < total { more } else { done };
                let separator = usize::from(last.is_some());
                if overhead + bytes + separator + size > BUDGET {
                    if last.is_some() { break }
                    if first && (self.root_extras() || overhead + self.header(false).json_encoded_len() + size + 2 <= BUDGET) {
                        return Ok(self.title_page())
                    }
                    return self.fragment(Part::Node { at, source: subtree }, None, first)
                }
                if last.is_some() { source.push_static(" "); }
                source.push_builder(&subtree);
                bytes += separator + size;
                last = Some(node.entity.id);
                at = node.end;
            }
        }
        source.push_builder(&footer);
        let next = last.filter(|_| at < total).map(|id| AiJson::id(&id));
        Ok(env.reply(AiJson::built_str(source), next, None, None))
    }
}

pub fn read<'a>(state: &'a State, gate: &Gate<'a>, board: Id, rect: Option<[f64; 4]>, after: Option<Cursor>) -> Result<AiOut, String> {
    check_state(state, gate)?;
    let board = Board::new(state, gate, board, rect).map_err(|e| match after {
        Some(Cursor::Fragment { .. }) => STALE_FRAGMENT.to_owned(),
        Some(Cursor::After(_)) => STALE.to_owned(),
        None => e,
    })?;
    gate.finish_json(board.page(after)?).map_err(withheld)
}
