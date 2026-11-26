use makepad_work_core::entity::{field, kind, Entity};
use makepad_work_core::event::{Change, Id, Key, Value};
use makepad_work_core::fold::State;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) fn board_kind(kind: &Key) -> bool {
    match kind {
        Key::Number(id) => super::schema::Vocabulary::Board.schema().item(*id).is_some(),
        Key::Name(_) => true,
    }
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
