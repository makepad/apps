use super::index::{Rect, RectIndex};
use super::tree::{board_kind, live, parent, BoardTree};
use makepad_work_core::entity::{field, kind, Entity};
use makepad_work_core::event::{self, Content, Event, Id, Key, Value};
use makepad_work_core::fold::State;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Resolve UNDO (including redo) through the event log, e.g. `|id| log.get(id)`.
pub fn touched_by<'a>(mut event: &'a Event, lookup: impl Fn(&Id) -> Option<&'a Event>) -> Vec<Id> {
    let mut seen = HashSet::new();
    loop {
        if !matches!(event.header.kind, event::kind::ENTITY | event::kind::SET | event::kind::UNDO) {
            return Vec::new();
        }
        let content = event.content().and_then(|body| Content::decode(event.header.kind, event.header.kind_version, body));
        match content {
            Some(Content::Entity { .. }) => return vec![event.id],
            Some(Content::Set { changes, .. }) => {
                let mut ids: Vec<_> = changes.into_iter().map(|c| c.entity).collect();
                ids.sort_unstable();
                ids.dedup();
                return ids;
            }
            Some(Content::Undo { of }) if seen.insert(of) => {
                let Some(target) = lookup(&of) else { return Vec::new() };
                event = target;
            }
            _ => return Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct BoardItem {
    pub id: Id,
    pub kind: Key,
    pub rect: Rect,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub locked: bool,
    pub selectable: bool,
    pub parent_section: Option<Id>,
    pub colour: Option<u32>,
    pub z_key: Option<Box<[u8]>>,
    z_writer: Option<Id>,
}

impl BoardItem {
    pub fn text_handle(&self) -> Id {
        self.id
    }

    fn read(entity: &Entity, parent_section: Option<Id>) -> Self {
        let coord = |field| match entity.value(field) {
            Some(Value::Int(value)) => i128::from(*value),
            _ => 0,
        };
        let (x, y) = (coord(field::X), coord(field::Y));
        let size = |key, fallback: i128| match entity.value(key) {
            Some(Value::Int(n)) => i128::from(*n),
            _ if matches!(entity.kind, Key::Name(_)) => fallback * 64,
            _ => 0,
        };
        let (w, h) = (size(field::W, 160), size(field::H, 64));
        let world = |n: i128| n as f64 / 64.0;
        let z_key = entity.z_key().map(|(bytes, _)| Box::from(bytes));
        Self {
            id: entity.id,
            kind: entity.kind.clone(),
            rect: Rect::new(world(x), world(y), world(x + w), world(y + h)),
            x: x as i64,
            y: y as i64,
            w: w as i64,
            h: h as i64,
            locked: entity.flag(field::LOCKED),
            selectable: true,
            parent_section,
            colour: match entity.value(field::COLOUR) {
                Some(Value::Int(n)) => u32::try_from(*n).ok(),
                _ => None,
            },
            z_writer: z_key.as_ref().map(|_| entity.fields[&field::Z].writer),
            z_key,
        }
    }
}

// Late events can shift canonical positions without changing relative order. Read the current
// writer positions only when placing equal keys. Drawing and hit testing use the resulting ranks.
fn compare(a: &BoardItem, b: &BoardItem, state: &State) -> Ordering {
    a.z_key.cmp(&b.z_key).then_with(|| {
        if a.z_key.is_some() && a.z_writer != b.z_writer {
            state.entities[&a.id].z_key().unwrap().1.cmp(&state.entities[&b.id].z_key().unwrap().1)
        } else {
            Ordering::Equal
        }
    }).then_with(|| a.id.cmp(&b.id))
}

pub struct BoardView {
    board: Id,
    board_live: bool,
    by_id: HashMap<Id, u32>,
    items: Vec<Option<BoardItem>>,
    free: Vec<u32>,
    index: RectIndex,
    order: Vec<u32>,
    ranks: Vec<usize>,
    // Parent is mutable. A section moved between boards brings its subtree along in one SET.
    parents: HashMap<Id, Id>,
    children: HashMap<Id, HashSet<Id>>,
    inactive_sections: HashSet<Id>,
}

impl BoardView {
    pub fn build(state: &State, board: Id) -> Self {
        let mut view = Self {
            board, board_live: state.entities.get(&board).is_some_and(|e| e.kind == kind::BOARD && live(e)),
            by_id: HashMap::new(), items: Vec::new(), free: Vec::new(),
            index: RectIndex::new(), order: Vec::new(), ranks: Vec::new(),
            parents: HashMap::new(), children: HashMap::new(), inactive_sections: HashSet::new(),
        };
        let mut tree = BoardTree::new(state);
        for entity in state.entities.values().filter(|e| board_kind(&e.kind)) {
            view.set_parent(entity.id, parent(entity));
            if entity.kind == kind::SECTION && !live(entity) {
                view.inactive_sections.insert(entity.id);
            }
            if view.board_live && live(entity) {
                if let Some(p) = tree.placement(entity).filter(|p| p.board == board) {
                    let handle = view.insert(BoardItem::read(entity, p.section));
                    view.order.push(handle);
                }
            }
        }
        let items = &view.items;
        view.order.sort_unstable_by(|&a, &b| compare(items[a as usize].as_ref().unwrap(), items[b as usize].as_ref().unwrap(), state));
        view.rank_from(0);
        view
    }

    fn set_parent(&mut self, id: Id, parent: Option<Id>) -> bool {
        if self.parents.get(&id).copied() == parent {
            return false;
        }
        if let Some(old) = self.parents.remove(&id) {
            let siblings = self.children.get_mut(&old).unwrap();
            siblings.remove(&id);
            if siblings.is_empty() {
                self.children.remove(&old);
            }
        }
        if let Some(new) = parent {
            self.parents.insert(id, new);
            self.children.entry(new).or_default().insert(id);
        }
        true
    }

    fn insert(&mut self, item: BoardItem) -> u32 {
        let handle = self.free.pop().unwrap_or_else(|| {
            let handle = u32::try_from(self.items.len()).expect("board handle space exhausted");
            self.items.push(None);
            self.ranks.push(usize::MAX);
            handle
        });
        self.by_id.insert(item.id, handle);
        self.index.insert(handle, item.rect);
        self.items[handle as usize] = Some(item);
        handle
    }

    fn rank_from(&mut self, first: usize) {
        for (at, &handle) in self.order.iter().enumerate().skip(first) {
            self.ranks[handle as usize] = at;
        }
    }

    /// Apply the ids of each folded event, including any newly ready events. Coordinates are
    /// absolute, so changing section geometry never requires visiting its members. Returns the
    /// ids of records inserted, refreshed (including text-only edits), or removed on this board.
    pub fn apply(&mut self, state: &State, touched: &[Id]) -> HashSet<Id> {
        let mut affected: HashSet<_> = touched.iter().copied().collect();
        let mut moved = Vec::new();
        for &id in touched {
            if id == self.board {
                let board_live = state.entities.get(&id).is_some_and(|e| e.kind == kind::BOARD && live(e));
                if self.board_live != board_live {
                    // Looped items can belong here without a parent path to the board.
                    let old = std::mem::replace(self, Self::build(state, id));
                    // Only the live side of this transition has records.
                    return old.by_id.into_keys().chain(self.by_id.keys().copied()).collect();
                }
            }
            let entity = state.entities.get(&id).filter(|e| board_kind(&e.kind));
            let inactive_changed = if entity.is_some_and(|e| e.kind == kind::SECTION && !live(e)) {
                self.inactive_sections.insert(id)
            } else {
                self.inactive_sections.remove(&id)
            };
            let reparented = self.set_parent(id, entity.and_then(parent));
            if (reparented || inactive_changed) && self.children.contains_key(&id) {
                moved.push(id);
            }
        }
        let mut visited = HashSet::new();
        while let Some(id) = moved.pop() {
            if visited.insert(id) {
                for &child in self.children.get(&id).into_iter().flatten() {
                    affected.insert(child);
                    if self.children.contains_key(&child) { moved.push(child); }
                }
            }
        }

        let mut detach = Vec::new();
        let mut place = Vec::new();
        let mut tree = BoardTree::new(state);
        affected.retain(|&id| {
            let item = state.entities.get(&id).filter(|e| self.board_live && board_kind(&e.kind) && live(e))
                .and_then(|e| tree.placement(e).filter(|p| p.board == self.board)
                    .map(|p| BoardItem::read(e, p.section)));
            match (self.by_id.get(&id).copied(), item) {
                (Some(h), Some(item)) => {
                    let old = self.item(h);
                    if item.z_writer != old.z_writer || item.z_key != old.z_key {
                        detach.push(self.ranks[h as usize]);
                        place.push(h);
                    }
                    if item.rect != old.rect { self.index.update(h, item.rect); }
                    self.items[h as usize] = Some(item);
                }
                (Some(h), None) => {
                    self.by_id.remove(&id);
                    self.index.remove(h);
                    self.items[h as usize] = None;
                    self.free.push(h);
                    detach.push(self.ranks[h as usize]);
                }
                (None, Some(item)) => place.push(self.insert(item)),
                (None, None) => return false,
            }
            true
        });
        // Remove all changed keys first: only the untouched subsequence is still sorted.
        let mut first = self.order.len();
        detach.sort_unstable();
        for at in detach.into_iter().rev() {
            self.order.remove(at);
            first = first.min(at);
        }
        for h in place {
            let at = self.order.partition_point(|&other| compare(self.item(other), self.item(h), state).is_lt());
            self.order.insert(at, h);
            first = first.min(at);
        }
        self.rank_from(first);
        affected
    }

    fn item(&self, handle: u32) -> &BoardItem {
        self.items[handle as usize].as_ref().unwrap()
    }

    pub fn get(&self, id: &Id) -> Option<&BoardItem> {
        self.by_id.get(id).map(|&h| self.item(h))
    }

    pub fn hit(&self, (x, y): (f64, f64), tolerance: f64) -> Option<Id> {
        let t = tolerance.max(0.0);
        let mut top = None;
        self.index.visit(Rect::new(x - t, y - t, x + t, y + t), |h| {
            let rank = self.ranks[h as usize];
            if self.item(h).selectable && top.is_none_or(|r| rank > r) { top = Some(rank); }
        });
        top.map(|rank| self.item(self.order[rank]).id)
    }

    pub fn marquee(&self, rect: Rect) -> Vec<Id> {
        let mut ids = self.visible(rect);
        ids.retain(|id| {
            let item = self.get(id).unwrap();
            item.selectable && (item.kind != kind::SECTION || rect.encloses(item.rect))
        });
        ids
    }

    pub fn visible(&self, rect: Rect) -> Vec<Id> {
        let mut hits = Vec::new();
        self.index.query_rect(rect, &mut hits);
        hits.sort_unstable_by_key(|&h| self.ranks[h as usize]);
        hits.into_iter().map(|h| self.item(h).id).collect()
    }

    /// Contiguous ranges of the supplied z-ordered visible set. Only the current layer can
    /// conflict, so even a stack of identical rects compares against just its last member.
    pub fn layers(&self, visible: &[Id]) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut layer = RectIndex::new();
        let mut hits = Vec::new();
        let mut start = 0;
        for (at, id) in visible.iter().enumerate() {
            let h = self.by_id[id];
            let rect = self.item(h).rect;
            layer.query_rect(rect, &mut hits);
            if hits.iter().any(|&other| rect.overlaps(self.item(other).rect)) {
                ranges.push(start..at);
                start = at;
                layer = RectIndex::new();
            }
            layer.insert(h, rect);
        }
        if start < visible.len() { ranges.push(start..visible.len()); }
        ranges
    }
}
