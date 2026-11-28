use makepad_town_core::entity::{field, kind, Entity};
use makepad_town_core::event::{Change, Id, Key, Value};
use makepad_town_core::fold::State;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) fn board_kind(kind: &Key) -> bool {
    match kind {
        Key::Number(id) => super::schema::Vocabulary::Board.schema().item(*id).is_some(),
        Key::Name(name) => name != super::schema::STEP,
    }
}

/// A presenting step: a child of a board, outside its items.
pub(crate) fn step_kind(kind: &Key) -> bool {
    matches!(kind, Key::Name(name) if name == super::schema::STEP)
}

/// The live steps whose parent is `board`, in path order.
pub(crate) fn steps(state: &State, board: Id) -> Vec<&Entity> {
    let mut steps: Vec<&Entity> = state.entities.values()
        .filter(|e| step_kind(&e.kind) && live(e) && parent(e) == Some(board))
        .collect();
    steps.sort_unstable_by(|a, b| a.order_key().cmp(&b.order_key()).then_with(|| a.id.cmp(&b.id)));
    steps
}

/// Where a step of a board's path leads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The whole board.
    Board,
    /// A live item (or section) on this board.
    Item(Id),
    /// An item of this board that is deleted or archived: skipped while presenting and struck
    /// through in the list.
    Gone(Id),
    /// No target, or an id that is not an item of this board (another board's item, a task).
    Unknown(Option<Id>),
}

/// One step of a board's presenting path, in path order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub id: Id,
    pub target: Target,
    pub title: String,
}

/// The board's presenting path: its live steps in order key order (the same order on every
/// side), each with where it leads. A step is never an error: a missing target is a state.
pub(crate) fn path(state: &State, board: Id) -> Vec<Step> {
    let mut tree = BoardTree::new(state);
    steps(state, board).into_iter().map(|step| {
        let target = match id_value(step.value(field::TO)) {
            None => Target::Unknown(None),
            Some(id) if id == board => Target::Board,
            // Placement follows the parent chain, so a deleted item still has a place: its own
            // flag decides whether the step can show it.
            Some(id) => match state.entities.get(&id).filter(|e| board_kind(&e.kind)).map(|e| (e, tree.placement(e))) {
                Some((item, Some(p))) if p.board == board && live(item) => Target::Item(id),
                Some((_, Some(p))) if p.board == board => Target::Gone(id),
                _ => Target::Unknown(Some(id)),
            },
        };
        Step { id: step.id, target, title: step.text(field::TITLE).to_owned() }
    }).collect()
}

fn id_value(value: Option<&Value>) -> Option<Id> {
    match value { Some(Value::Id(id)) => Some(*id), _ => None }
}

pub(crate) fn parent(item: &Entity) -> Option<Id> { id_value(item.value(field::PARENT)) }

pub(crate) fn live(item: &Entity) -> bool { !item.flag(field::DELETED) && !item.flag(field::ARCHIVED) }

#[derive(Clone, Copy)]
pub(crate) struct Placement {
    pub board: Id,
    pub section: Option<Id>,
}

#[derive(Clone, Copy)]
enum Root {
    Board { id: Id, live: bool, hanging: bool },
    Cycle,
    Missing,
}

// Cache a section's path for this snapshot only. Even a whole-board read with deep nesting
// visits each section link once. A cycle caches the failure, never another item's home board.
pub(crate) struct BoardTree<'a> {
    state: &'a State,
    new_parents: BTreeMap<Id, Option<Id>>,
    roots: HashMap<Id, Root>,
}

impl<'a> BoardTree<'a> {
    pub fn new(state: &'a State) -> Self {
        Self { state, new_parents: BTreeMap::new(), roots: HashMap::new() }
    }

    fn parent(&self, item: &Entity) -> Option<Id> {
        self.new_parents.get(&item.id).copied().unwrap_or_else(|| parent(item))
    }

    fn root(&mut self, mut next: Option<Id>) -> Root {
        let mut path = Vec::new();
        let mut root = loop {
            let Some(entity) = next.and_then(|id| self.state.entities.get(&id)) else { break Root::Missing };
            match entity.kind {
                Key::Number(kind::BOARD) => break Root::Board { id: entity.id, live: live(entity), hanging: false },
                Key::Number(kind::SECTION) => {
                    if let Some(&known) = self.roots.get(&entity.id) { break known }
                    // A second visit before unwinding this path means a cycle.
                    self.roots.insert(entity.id, Root::Cycle);
                    path.push((entity.id, live(entity)));
                    next = self.parent(entity);
                }
                _ => break Root::Missing,
            }
        };
        for (id, active) in path.into_iter().rev() {
            if let Root::Board { hanging, .. } = &mut root { *hanging |= !active; }
            self.roots.insert(id, root);
        }
        root
    }

    pub fn placement(&mut self, item: &Entity) -> Option<Placement> {
        let parent = self.parent(item);
        match self.root(parent) {
            Root::Board { id, live: true, hanging } => Some(Placement {
                board: id, section: parent.filter(|p| !hanging && *p != id),
            }),
            Root::Cycle => {
                // A child concurrently added to a moved section keeps its creator's home
                // board. Its current chain wins until it loops, then only its own home counts.
                let board = id_value(item.value(field::BOARD))?;
                self.state.entities.get(&board).filter(|b| b.kind == kind::BOARD && live(b))?;
                Some(Placement { board, section: None })
            }
            _ => None,
        }
    }

    /// The live children the writer sees, including nested sections.
    pub fn children(&mut self) -> HashMap<Id, Vec<Id>> {
        let mut children = HashMap::<_, Vec<_>>::new();
        for item in self.state.entities.values().filter(|e| board_kind(&e.kind) && live(e)) {
            if let Some(section) = self.placement(item).and_then(|p| p.section) {
                children.entry(section).or_default().push(item.id);
            }
        }
        children
    }
}

pub(crate) fn descendants(mut ids: BTreeSet<Id>, children: &HashMap<Id, Vec<Id>>) -> BTreeSet<Id> {
    let mut pending: Vec<_> = ids.iter().copied().collect();
    while let Some(id) = pending.pop() {
        for &child in children.get(&id).into_iter().flatten() {
            if ids.insert(child) { pending.push(child); }
        }
    }
    ids
}

/// Complete a locally authored PARENT edit before signing. A received SET already names
/// exactly the descendants its writer saw. Never expand it using the receiver's state.
pub(crate) fn add_home_changes(state: &State, changes: &mut Vec<Change>) {
    let mut after = BoardTree::new(state);
    for change in changes.iter().filter(|c| c.field == field::PARENT) {
        if state.entities.get(&change.entity).is_some_and(|e| board_kind(&e.kind)) {
            after.new_parents.insert(change.entity, id_value(Some(&change.value)));
        }
    }
    if after.new_parents.is_empty() { return }
    let mut affected: BTreeSet<_> = after.new_parents.keys().copied().collect();
    if affected.iter().any(|id| state.entities[id].kind == kind::SECTION) {
        // Include raw descendants even through a tombstone: their chain still follows it.
        let mut children = HashMap::<_, Vec<_>>::new();
        for item in state.entities.values().filter(|e| board_kind(&e.kind)) {
            if let Some(id) = parent(item) { children.entry(id).or_default().push(item.id); }
        }
        affected = descendants(affected, &children);
    }
    let mut before = BoardTree::new(state);
    for id in affected {
        let item = &state.entities[&id];
        let home = id_value(item.value(field::BOARD));
        let old = match before.root(parent(item)) {
            Root::Board { id, .. } => Some(id),
            Root::Cycle => home,
            Root::Missing => None,
        };
        let destination = match after.root(after.parent(item)) {
            Root::Board { id, .. } if old != Some(id) || home.is_none() => Some(id),
            // Older logs lack BOARD. Capture the known board before reparenting loses it.
            Root::Cycle if home.is_none() => old,
            _ => None,
        };
        if let Some(board) = destination {
            changes.push(Change { entity: id, field: field::BOARD, value: Value::Id(board) });
        }
    }
}
