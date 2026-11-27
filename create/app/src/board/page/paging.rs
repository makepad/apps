use super::{string, Board};
use makepad_ai_services::wire::MAX_RESULT_BYTES;
use makepad_town_core::entity::field;
use makepad_town_core::event::Id;
use makepad_town_core::fold::State;
use makepad_town_core::{hex, parse_id};
use makepad_network::digest::sha256_hash;
use makepad_strict_json as json;

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

// Stored literals can contain raw whitespace and control characters. Match the wire JSON writer.
fn encoded_char_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\x08' | '\x0c' => 2,
        c if c < ' ' => 6,
        c => c.len_utf8(),
    }
}

fn encoded_len(s: &str) -> usize { s.chars().map(encoded_char_len).sum() }

struct Envelope {
    head: String,
}

impl Envelope {
    fn new(board: &Board, parent: Id) -> Self {
        Self { head: format!(r#"{{"board":"{}","revision":"{}","total":{},"parent":"{}","splash":"#,
            hex(&board.root.id), board.revision, board.nodes.len(), hex(&parent)) }
    }

    fn reply(&self, source: &str, next: Option<&str>, extra: &str) -> String {
        let mut reply = self.head.clone();
        json::s(source).write_into(&mut reply);
        reply.push_str(extra);
        reply.push_str(r#","next":"#);
        if let Some(cursor) = next { json::s(cursor).write_into(&mut reply); }
        else { reply.push_str("null"); }
        reply.push('}');
        reply
    }
}

enum Part { Title, Node { at: usize, source: String } }

impl Board<'_> {
    fn envelope(&self, at: usize) -> Envelope {
        let parent = self.nodes.get(at).and_then(|n| n.parent).map_or(self.root.id, |i| self.nodes[i].entity.id);
        Envelope::new(self, parent)
    }

    fn title_page(&self) -> String {
        let mut title = self.header(true);
        title.push_str(&self.footer(true));
        self.envelope(0).reply(&title, Some(&hex(&self.root.id)), "")
    }

    fn fragment(&self, part: Part, cursor: Option<(Id, usize)>, with_title: bool) -> Result<String, String> {
        let (id, source, last, end, kind, env) = match part {
            Part::Title if self.root_extras() => {
                let source = self.header(true) + &self.footer(true);
                (self.root.id, source, self.root.id, 0, "root", Envelope::new(self, self.root.id))
            }
            Part::Title => {
                let mut source = String::new();
                string(&mut source, self.root.text(field::TITLE));
                (self.root.id, source, self.root.id, 0, "title", Envelope::new(self, self.root.id))
            }
            Part::Node { at, source } => {
                let node = &self.nodes[at];
                (node.entity.id, source, node.entity.id, node.end, "node", self.envelope(at))
            }
        };
        let digest = sha256_hash(source.as_bytes());
        let index = match cursor {
            Some((hash, index)) if hash == digest => index,
            Some(_) => return Err(STALE_FRAGMENT.into()),
            None => 1,
        };
        let id = hex(&id);
        let hash = hex(&digest);
        let continuation = |i| format!("{id}~{hash}~{i}");
        let metadata = |i, count| format!(r#","fragment":{{"kind":"{kind}","id":"{id}","hash":"{hash}","index":{i},"total":{count}}}"#);

        // The split must survive changes to board total, revision, title and parent. All ids
        // have fixed width. Reserve the maximum decimal widths of the counters in the envelope.
        let worst = u64::MAX; // The wire split is identical on 32- and 64-bit peers.
        let overhead = env.reply("", Some(&continuation(worst)), &metadata(worst, worst)).len()
            + (worst.to_string().len() - self.nodes.len().to_string().len());
        let town = BUDGET - overhead;
        let mut bounds = vec![0];
        let mut bytes = 0;
        for (offset, c) in source.char_indices() {
            let width = encoded_char_len(c);
            if bytes + width > town { bounds.push(offset); bytes = 0; }
            bytes += width;
        }
        bounds.push(source.len());
        let count = bounds.len() - 1;
        if index == 0 || index > count { return Err(STALE_FRAGMENT.into()) }
        let piece = &source[bounds[index - 1]..bounds[index]];
        let next = if index < count { Some(continuation(index as u64 + 1)) }
            else if end < self.nodes.len() { Some(hex(&last)) } else { None };
        let mut extra = metadata(index as u64, count as u64);
        if with_title {
            extra.push_str(r#","title":"#);
            json::s(self.root.text(field::TITLE)).write_into(&mut extra);
        }
        let reply = env.reply(piece, next.as_deref(), &extra);
        if with_title && reply.len() > BUDGET { return Ok(self.title_page()) }
        debug_assert!(reply.len() <= BUDGET);
        Ok(reply)
    }

    fn page(&self, after: Option<Cursor>) -> Result<String, String> {
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
        let board_id = hex(&self.root.id);
        let more = env.reply("", Some(&board_id), "").len();
        let done = env.reply("", None, "").len();
        let mut source = self.header(first);
        let footer = self.footer(first);
        let mut bytes = encoded_len(&source) + encoded_len(&footer);
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
                let size = encoded_len(&subtree);
                let overhead = if node.end < total { more } else { done };
                let separator = usize::from(last.is_some());
                if overhead + bytes + separator + size > BUDGET {
                    if last.is_some() { break }
                    if first && (self.root_extras() || overhead + encoded_len(&self.header(false)) + size + 2 <= BUDGET) {
                        return Ok(self.title_page())
                    }
                    return self.fragment(Part::Node { at, source: subtree }, None, first)
                }
                if last.is_some() { source.push(' '); }
                source.push_str(&subtree);
                bytes += separator + size;
                last = Some(node.entity.id);
                at = node.end;
            }
        }
        source.push_str(&footer);
        let next = last.filter(|_| at < total).map(|id| hex(&id));
        Ok(env.reply(&source, next.as_deref(), ""))
    }
}

pub fn read(state: &State, board: Id, rect: Option<[f64; 4]>, after: Option<Cursor>) -> Result<String, String> {
    let board = Board::new(state, board, rect).map_err(|e| match after {
        Some(Cursor::Fragment { .. }) => STALE_FRAGMENT.to_owned(),
        Some(Cursor::After(_)) => STALE.to_owned(),
        None => e,
    })?;
    board.page(after)
}
