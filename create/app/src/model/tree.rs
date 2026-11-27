//! The town tree's snapshot: the open town, its projects, and what each project holds.
//! Rows carry ids; titles are display text only.

use super::{Action, Model, Origin, Town};
use crate::board::tree::live;
use makepad_town_core::entity::{field, kind, Entity};
use makepad_town_core::event::{Change, Content, Id, Key, Value};

pub const UNTITLED: &str = "Untitled town";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TownEntry {
    pub id: Id,
    pub title: String,
    pub note: String,
}

/// What a tree row opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The project's one task list. Its row carries the project's id.
    Tasks,
    Board,
    Doc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub id: Id,
    pub kind: RowKind,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeProject {
    pub id: Id,
    pub title: String,
    /// How many live tasks the project's one task list holds.
    pub tasks: usize,
    /// The task list, then the boards, then the docs, each in the order they were made.
    pub rows: Vec<TreeRow>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TownTree {
    pub town: Option<Id>,
    pub name: Option<String>,
    pub label: String,
    pub projects: Vec<TreeProject>,
}

impl TownTree {
    pub fn project(&self, id: Id) -> Option<&TreeProject> { self.projects.iter().find(|p| p.id == id) }

    /// The project that holds the row `id` (a board, a doc, or the task list's project id).
    pub fn project_of(&self, id: Id) -> Option<&TreeProject> {
        self.projects.iter().find(|p| p.id == id || p.rows.iter().any(|r| r.id == id))
    }

    pub fn row(&self, id: Id) -> Option<&TreeRow> {
        self.projects.iter().flat_map(|p| &p.rows).find(|r| r.id == id && r.kind != RowKind::Tasks)
    }
}

/// Stable order: when the entity was made, then its id. Activity never moves a row.
fn in_order<'a>(mut entities: Vec<&'a Entity>) -> Vec<&'a Entity> {
    entities.sort_unstable_by_key(|e| (e.created, e.id));
    entities
}

impl Town {
    // Entities are keyed by id. Concurrent names resolve to the lowest live TOWN id.
    pub(super) fn town_name_entity(&self) -> Option<&Entity> {
        self.state.entities.values().find(|e| e.kind == kind::TOWN && e.project.is_none() && live(e))
    }

    pub(super) fn title(&self) -> Option<&str> {
        self.town_name_entity().map(|e| e.text(field::TITLE)).filter(|name| !name.is_empty())
    }
}

impl Model {
    /// The town's own title, including for a known town that is currently closed.
    pub fn town_name(&self, id: Id) -> Option<&str> {
        if id == self.open_id() { return self.open.title() }
        if let Some(town) = self.background.get(&id) { return town.title() }
        self.towns.get(&id)?.name.as_deref()
    }

    /// A plain snapshot for the tree. Walks the folded entities once; call it when the model
    /// changes, never per frame. A board or doc whose project is deleted or missing isn't listed.
    pub fn town_tree(&self) -> TownTree {
        let town = self.open_id();
        let name = self.open.title().map(str::to_owned);
        let label = name.as_deref().unwrap_or(UNTITLED).to_owned();
        let live_of = |k: u16| in_order(self.state.entities.values().filter(|e| e.kind == k && live(e)).collect());
        let projects = live_of(kind::PROJECT);
        let (boards, docs) = (live_of(kind::BOARD), live_of(kind::DOC));
        let mut tasks = std::collections::BTreeMap::new();
        for task in self.state.entities.values().filter(|e| e.kind == kind::TASK && live(e)) {
            if let Some(project) = task.project { *tasks.entry(project).or_insert(0) += 1; }
        }
        let projects = projects.into_iter().map(|project| {
            let mut rows = vec![TreeRow { id: project.id, kind: RowKind::Tasks, title: "Tasks".into() }];
            for (kind, list) in [(RowKind::Board, &boards), (RowKind::Doc, &docs)] {
                rows.extend(list.iter().filter(|e| e.project == Some(project.id)).map(|e| TreeRow {
                    id: e.id, kind, title: e.text(field::TITLE).to_owned(),
                }));
            }
            TreeProject { id: project.id, title: project.text(field::TITLE).to_owned(),
                tasks: tasks.get(&project.id).copied().unwrap_or(0), rows }
        }).collect();
        TownTree { town: Some(town), name, label, projects }
    }

    /// Names the open town. The first name makes the `TOWN` entity; later ones edit the one
    /// that wins. An empty name (after trimming) is refused.
    pub fn rename_town(&mut self, title: &str) -> Result<(), String> {
        let title = title.trim();
        if title.is_empty() { return Err("a town name can't be empty".into()) }
        match self.town_name_entity().map(|e| (e.id, e.text(field::TITLE) == title)) {
            Some((_, true)) => Ok(()),
            Some((id, false)) => {
                let changes = vec![Change { entity: id, field: field::TITLE, value: Value::Text(title.into()) }];
                self.act(Action::SetFields { changes, label: Some("rename town".into()) }, Origin::Ui).map(|_| ())
            }
            None => {
                if !self.state.active_member(&self.me()) { return Err("This town is read-only now.".into()) }
                self.append(Content::Entity { kind: Key::Number(kind::TOWN), derive: None, project: None,
                    fields: vec![(field::TITLE, Value::Text(title.into()))] }, Origin::Ui).map(|_| ())
            }
        }
    }

    /// The write that retitles a live project, board or doc, known by its id. An empty title
    /// (after trimming) is refused; `None` when the title is already that.
    pub fn rename_action(&self, id: Id, title: &str) -> Result<Option<Action>, String> {
        let title = title.trim();
        if title.is_empty() { return Err("a title can't be empty".into()) }
        let entity = self.state.entities.get(&id)
            .filter(|e| matches!(e.kind.number(), Some(kind::PROJECT | kind::BOARD | kind::DOC)) && live(e))
            .ok_or("nothing to rename here")?;
        if entity.text(field::TITLE) == title { return Ok(None) }
        let label = match entity.kind.number() {
            Some(kind::PROJECT) => "rename project",
            Some(kind::BOARD) => "rename board",
            _ => "rename doc",
        };
        let changes = vec![Change { entity: id, field: field::TITLE, value: Value::Text(title.into()) }];
        Ok(Some(Action::SetFields { changes, label: Some(label.into()) }))
    }

    pub fn rename(&mut self, id: Id, title: &str) -> Result<(), String> {
        match self.rename_action(id, title)? {
            Some(action) => self.act(action, Origin::Ui).map(|_| ()),
            None => Ok(()),
        }
    }

    /// The existing `DELETED` write for a board or a doc, known by its id (undoable).
    pub fn delete_page_action(&self, id: Id) -> Result<Action, String> {
        let entity = self.state.entities.get(&id)
            .filter(|e| matches!(e.kind.number(), Some(kind::BOARD | kind::DOC)) && live(e))
            .ok_or("nothing to delete here")?;
        let label = if entity.kind.number() == Some(kind::BOARD) { "delete board" } else { "delete doc" };
        let changes = vec![Change { entity: id, field: field::DELETED, value: Value::Bool(true) }];
        Ok(Action::SetFields { changes, label: Some(label.into()) })
    }

    pub fn delete_page(&mut self, id: Id) -> Result<(), String> {
        let action = self.delete_page_action(id)?;
        self.act(action, Origin::Ui).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::ShareHomes;

    fn ids(project: &TreeProject) -> Vec<Id> { project.rows.iter().map(|r| r.id).collect() }

    #[test]
    fn the_tree_lists_each_projects_rows_and_follows_rename_and_delete() {
        let homes = ShareHomes::new("town-tree");
        let (mut model, _) = Model::open(&homes.0).unwrap();
        let tree = model.town_tree();
        assert_eq!(tree.town, Some(model.open_id()), "the tree names the open town by id");
        assert!(tree.projects.len() >= 2, "a fresh home seeds two projects");
        let (a, b) = (tree.projects[0].id, tree.projects[1].id);
        // Interleave creation across the projects and kinds.
        let doc_a = model.act(Action::AddDoc { title: "Notes".into(), project: a }, Origin::Ui).unwrap();
        let board_a = model.act(Action::AddBoard { title: "Layout".into(), project: a }, Origin::Ui).unwrap();
        let board_b = model.act(Action::AddBoard { title: "Wiring".into(), project: b }, Origin::Ui).unwrap();
        let doc_b = model.act(Action::AddDoc { title: "Plan".into(), project: b }, Origin::Ui).unwrap();
        let board_a2 = model.act(Action::AddBoard { title: "Second".into(), project: a }, Origin::Ui).unwrap();

        let tree = model.town_tree();
        assert_eq!(tree.projects[0].id, a, "projects keep their order");
        assert_eq!(tree.projects[1].id, b, "projects keep their order");
        assert_eq!(ids(&tree.projects[0])[1..], [board_a, board_a2, doc_a], "project a: boards then docs, in creation order");
        assert_eq!(ids(&tree.projects[1])[1..], [board_b, doc_b], "project b lists only its own rows");
        assert_eq!(tree.projects[0].rows[0].kind, RowKind::Tasks, "the task list leads each project");
        assert_eq!(tree.projects[0].rows[0].id, a, "the task list row carries its project's id");
        model.act(Action::AddTask { title: "Sketch".into(), text: String::new() }, Origin::Ui).unwrap();
        let counted = model.town_tree().projects.iter().map(|p| p.tasks).sum::<usize>();
        assert_eq!(counted, 1, "the task list counts its live tasks");

        model.rename(board_a, "  Print layout ").unwrap();
        let tree = model.town_tree();
        let row = tree.row(board_a).unwrap();
        assert_eq!((row.title.as_str(), row.id), ("Print layout", board_a), "a rename trims, changes the title and keeps the id");
        assert!(model.rename(board_a, "   ").is_err(), "an empty title is refused");
        let events = model.log.events().len();
        model.rename(board_a, "Print layout").unwrap();
        assert_eq!(model.log.events().len(), events, "an unchanged title writes nothing");
        assert_eq!(model.town_tree().row(board_a).unwrap().title, "Print layout", "a refused rename keeps the title");
        model.rename(a, "Fab").unwrap();
        assert_eq!(model.town_tree().project(a).unwrap().title, "Fab", "a project renames too");

        model.delete_page(board_a).unwrap();
        assert!(model.town_tree().row(board_a).is_none(), "a deleted board leaves the tree");
        assert!(model.delete_page(a).is_err(), "a project isn't deleted as a page");
        let town = model.open_id();
        let kept = model.town_tree();
        drop(model);
        let (mut model, _) = Model::open(&homes.0).unwrap();
        model.open_town(town).unwrap();
        assert_eq!(model.town_tree(), kept, "the deleted board stays gone and the rest is the same after reopening");

        // A board whose project is deleted isn't listed.
        let project = model.act(Action::AddProject { title: "Gone".into() }, Origin::Ui).unwrap();
        model.act(Action::AddBoard { title: "Orphan".into(), project }, Origin::Ui).unwrap();
        let gone = vec![Change { entity: project, field: field::DELETED, value: Value::Bool(true) }];
        model.act(Action::SetFields { changes: gone, label: None }, Origin::Ui).unwrap();
        let tree = model.town_tree();
        assert!(tree.project(project).is_none() && tree.projects.iter().all(|p| p.rows.iter().all(|r| r.title != "Orphan")),
            "a board under a deleted project isn't listed");
    }

    #[test]
    fn the_town_name_is_a_synced_field_and_the_lowest_id_wins() {
        use crate::model::tests::{exchange, join, ShareHomes};
        // A town started for sharing has no name yet; a new home's own first town is "Center".
        let homes = ShareHomes::new("town-name");
        let (mut a, mut b) = homes.pair();
        join(&mut a, &mut b, true);
        let town = b.open_id();
        let offer = makepad_town_core::share::Message::Offer { town, offer: a.this_offer() }.encode();
        assert!(b.sync_receive(&offer).unwrap());
        assert_eq!(b.offer_prompt_line(), "Ada wants to add a town to this computer.");
        assert!(b.answer_prompt(b.offer_prompt().unwrap(), false));
        let derived = a.town_tree().label;
        let listed = a.towns().into_iter().find(|row| row.id == a.open_id()).unwrap();
        assert_eq!(derived, listed.title, "an unnamed town has the same title on the page and in the rail");
        assert_eq!(listed.note, format!("with {}", a.device_label(&b.me())), "sharing status stays separate from the title");
        assert!(a.rename_town("   ").is_err(), "an empty name is refused");
        // Both computers name the town before they hear of each other's entity.
        a.rename_town("  From a ").unwrap();
        assert_eq!(a.town_tree().label, "From a", "the tree row and Home follow the first rename");
        a.rename_town("From a").unwrap();
        b.rename_town("From b").unwrap();
        let count = |m: &Model| m.state.entities.values().filter(|e| e.kind == kind::TOWN).count();
        assert_eq!((count(&a), count(&b)), (1, 1), "a later rename edits the one entity");
        exchange(&mut a, &mut b);
        assert_eq!((count(&a), count(&b)), (2, 2), "both entities fold on both sides");
        let winner = a.state.entities.values().filter(|e| e.kind == kind::TOWN).map(|e| e.id).min().unwrap();
        assert_eq!(a.town_name_entity().map(|e| e.id), Some(winner), "the lowest id is the name");
        assert_eq!(a.town_tree().label, b.town_tree().label, "both sides read the same name");
        b.rename_town("Final").unwrap();
        exchange(&mut a, &mut b);
        assert_eq!((a.town_tree().label.as_str(), b.town_tree().label.as_str()), ("Final", "Final"), "later renames edit the winner");
        assert_eq!(a.state.entities[&winner].text(field::TITLE), "Final", "and it is the winning entity that changed");

        let private = b.towns().into_iter().map(|row| row.id).find(|id| *id != town).unwrap();
        b.open_town(private).unwrap();
        a.rename_town("Studio").unwrap();
        exchange(&mut a, &mut b);
        assert!(b.sync_receive(&offer).unwrap());
        assert_eq!(b.offer_prompt_line(), "Ada wants to add \"Studio\" to this computer.", "the offer names the background town");
        b.sync_stop();
        assert_eq!(b.town_name(town), Some("Studio"), "a closed town retains its latest folded name");
    }
}
