use super::index::{Rect, RectIndex};
use super::tree::{board_kind, live, parent, BoardTree};
pub use super::tree::{Step, Target};
use makepad_town_core::entity::{field, kind, Entity};
use makepad_town_core::event::{self, Content, Event, Id, Key, Value};
use makepad_town_core::fold::State;
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
pub(super) struct BoardImage {
    pub blob: Option<(Id, u64)>,
    pub poster: Option<(Id, u64)>,
    pub media: String,
    pub duration_ms: u64,
    pub duration_label: String,
}

impl BoardImage {
    fn read(e: &Entity) -> Self {
        let file = |key| match e.value(key) {
            Some(Value::Blob { size, root }) => Some((makepad_town_core::blob::id(*size, root), *size)),
            _ => None,
        };
        let duration_ms = match e.value(field::DURATION) {
            Some(Value::Int(n)) => u64::try_from(*n).unwrap_or(0),
            _ => 0,
        };
        let secs = duration_ms / 1000;
        Self { blob: file(field::BLOB), poster: file(field::POSTER), media: e.text(field::MEDIA).into(), duration_ms,
            duration_label: if duration_ms > 0 { format!("{}:{:02}", secs / 60, secs % 60) } else { String::new() } }
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
    pub(super) image: Option<Box<BoardImage>>,
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
            image: (entity.kind == kind::IMAGE).then(|| Box::new(BoardImage::read(entity))),
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

    /// Top-left in board ticks for a new card. Search beyond the viewport too when
    /// placing beside a visible item, since its neighbour may be just off screen.
    pub fn next_free_spot(&self, viewport: Option<Rect>, size: (f64, f64)) -> (i64, i64) {
        let by_right = |a: &&BoardItem, b: &&BoardItem| a.rect.x1.total_cmp(&b.rect.x1).then(a.id.cmp(&b.id));
        let ticks = |v: f64| (v * 64.).round() as i64;
        let beside = |item: &BoardItem| (item.rect.x1 + 32., item.rect.y0);
        let Some(vp) = viewport else {
            return self.items.iter().flatten().max_by(by_right)
                .map_or((0, 0), |item| { let (x, y) = beside(item); (ticks(x), ticks(y)) });
        };
        let overlaps = |x, y| {
            let candidate = Rect::new(x, y, x + size.0, y + size.1);
            self.visible(candidate).into_iter().filter_map(|id| self.get(&id))
                .filter(|item| item.rect.overlaps(candidate)).max_by(by_right)
        };
        let (mut x, mut y) = ((vp.x0 + vp.x1) * 0.5, (vp.y0 + vp.y1) * 0.5);
        x = ticks(x) as f64 / 64.;
        y = ticks(y) as f64 / 64.;
        if let Some(obstacle) = overlaps(x, y) {
            let rightmost = self.visible(vp).into_iter().filter_map(|id| self.get(&id)).max_by(by_right);
            (x, y) = beside(rightmost.unwrap_or(obstacle));
            while let Some(item) = overlaps(x, y) { x = item.rect.x1 + 32.; }
        }
        (ticks(x), ticks(y))
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

impl BoardView {
    /// This board's presenting path, in order. A deleted board has none.
    pub fn path(&self, state: &State) -> Vec<Step> {
        if self.board_live { super::tree::path(state, self.board) } else { Vec::new() }
    }

    /// Each step of `path` with the world rect it brings into view, in path order; `None` for a
    /// step with nothing to show. The board step shows every item; an empty board shows nothing.
    pub fn present_targets(&self, path: &[Step]) -> Vec<(Id, Option<Rect>)> {
        let mut board = None;
        path.iter().map(|step| (step.id, match step.target {
            Target::Item(id) => self.get(&id).map(|item| item.rect),
            Target::Board => *board.get_or_insert_with(|| {
                self.order.iter().map(|&h| self.item(h).rect)
                    .reduce(|a, b| Rect::new(a.x0.min(b.x0), a.y0.min(b.y0), a.x1.max(b.x1), a.y1.max(b.y1)))
            }),
            Target::Gone(_) | Target::Unknown(_) => None,
        })).collect()
    }
}

/// Presenting a board along its path, for one view: which step shows, the glide toward it,
/// and the camera to go back to. None of it is in the fold. Steps whose target can't be shown
/// (deleted, unknown) are skipped; a path with nothing to show stays where it is. The path can
/// change while presenting: the step is kept by id, and if a peer deletes it, Next and
/// Previous go on from where it was.
#[derive(Clone)]
pub struct Present {
    at: Option<(usize, Id)>,
    before: makepad_canvas::Camera,
    glide: Option<(makepad_canvas::GlidePath, f64, f64)>,
    shown: makepad_canvas::Camera,
}

/// The margin a presented target keeps from the window's edges, in pixels.
const PRESENT_MARGIN: f64 = 24.0;

impl Present {
    /// Start presenting from `camera`, which Esc restores.
    pub fn start(camera: makepad_canvas::Camera) -> Self {
        Self { at: None, before: camera, glide: None, shown: camera }
    }

    /// The step shown (or being glided to); `None` before the first.
    pub fn step(&self) -> Option<Id> {
        self.at.map(|(_, id)| id)
    }

    /// The camera at `now`: along the glide while one runs, else the last arrival.
    pub fn camera(&self, now: f64) -> makepad_canvas::Camera {
        match &self.glide {
            Some((path, start, seconds)) if *seconds > 0.0 => path.at((now - start) / seconds),
            _ => self.shown,
        }
    }

    /// Whether a glide is still under way at `now` (the canvas keeps drawing while it is).
    pub fn gliding(&self, now: f64) -> bool {
        self.glide.as_ref().is_some_and(|(_, start, seconds)| now < start + seconds)
    }

    // Where the current step is in `targets`: its index, or where it was if it is gone.
    fn place(&self, targets: &[(Id, Option<Rect>)]) -> Option<(usize, bool)> {
        let (at, id) = self.at?;
        Some(match targets.iter().position(|(step, _)| *step == id) {
            Some(i) => (i, true),
            None => (at.min(targets.len()), false),
        })
    }

    /// Glide to the next step that has something to show, from `present_targets`. Returns
    /// whether it moved; at the end it holds.
    pub fn next(&mut self, targets: &[(Id, Option<Rect>)], now: f64) -> bool {
        let from = match self.place(targets) { None => 0, Some((i, here)) => i + usize::from(here) };
        let found = (from..targets.len()).find(|&i| targets[i].1.is_some());
        self.go(found, targets, now)
    }

    /// Glide to the previous step that has something to show.
    pub fn previous(&mut self, targets: &[(Id, Option<Rect>)], now: f64) -> bool {
        let Some((before, _)) = self.place(targets) else { return false };
        let found = (0..before).rev().find(|&i| targets[i].1.is_some());
        self.go(found, targets, now)
    }

    fn go(&mut self, found: Option<usize>, targets: &[(Id, Option<Rect>)], now: f64) -> bool {
        let Some((step, Some(rect))) = found.map(|i| targets[i]) else { return false };
        let from = self.camera(now);
        let Some(to) = fit_to(from.view, rect) else { return false };
        let path = makepad_canvas::GlidePath::new(from, to);
        self.glide = Some((path, now, path.seconds()));
        self.shown = to;
        self.at = Some((found.unwrap(), step));
        true
    }

    /// Keep the shown step framed in `view` after the window resized or its target moved or
    /// changed size. A resize lands at once; a moved target is glided to if a glide is under
    /// way. Returns whether the camera changed.
    pub fn refit(&mut self, targets: &[(Id, Option<Rect>)], view: makepad_widgets::Rect, now: f64) -> bool {
        let resized = self.shown.view != view;
        self.before.view = view;
        let here = self.place(targets).filter(|&(_, here)| here).and_then(|(i, _)| targets[i].1);
        let to = match here.and_then(|rect| fit_to(view, rect)) {
            Some(to) => to,
            None if resized => {
                let mut kept = self.shown;
                kept.view = view;
                kept
            }
            None => return false,
        };
        if !resized && to.scale == self.shown.scale && to.pan == self.shown.pan {
            return false;
        }
        self.glide = if !resized && self.gliding(now) {
            let path = makepad_canvas::GlidePath::new(self.camera(now), to);
            Some((path, now, path.seconds()))
        } else {
            None
        };
        self.shown = to;
        true
    }

    /// Stop presenting: the camera that was there before it started.
    pub fn end(self) -> makepad_canvas::Camera {
        self.before
    }
}

/// The camera that frames `rect` in `view` with the presenting margin, at a zoom the canvas can
/// show.
fn fit_to(view: makepad_widgets::Rect, rect: Rect) -> Option<makepad_canvas::Camera> {
    let world = makepad_widgets::Rect {
        pos: makepad_widgets::dvec2(rect.x0, rect.y0),
        size: makepad_widgets::dvec2((rect.x1 - rect.x0).max(1.0), (rect.y1 - rect.y0).max(1.0)),
    };
    let m = PRESENT_MARGIN;
    let insets = makepad_widgets::Inset { left: m, right: m, top: m, bottom: m };
    let fitted = makepad_canvas::Camera::fit(view, world, insets)?;
    let centre = fitted.local_to_screen(fitted.world_to_local(world.center()));
    fitted.zoom_about(centre, fitted.scale.clamp(makepad_canvas::MIN_ZOOM, makepad_canvas::MAX_ZOOM))
}
