//! World-space records and spatial queries shared by board drawing and input.

pub mod canvas;
pub(crate) mod design;
mod design_picture;
pub mod gesture;
pub mod index;
pub mod live;
mod look;
pub mod media;
pub mod page;
pub mod schema;
pub(crate) mod tree;
pub mod view;

pub use index::{Rect, RectIndex};
pub use view::{touched_by, BoardItem, BoardView};

#[cfg(test)]
mod tests;

use makepad_town_core::{event::Id, fold::State};
use std::collections::HashMap;

/// The tabs use the exact board ids displayed with their titles. Remember the
/// chosen id even while its creation is undone, so the empty canvas can redo it.
#[derive(Default)]
pub struct BoardPicker {
    pub boards: Vec<Id>,
    pub labels: Vec<String>,
    choices: HashMap<Id, Id>,
    // Undoing a creation can reveal a different board behind it.
    hidden_redo: HashMap<Id, Id>,
}

impl BoardPicker {
    pub fn choose(&mut self, project: Id, board: Id) {
        self.hidden_redo.remove(&project);
        self.choices.insert(project, board);
    }

    fn choice(&self, project: Id) -> Option<Id> {
        self.choices.get(&project).copied()
    }

    pub fn history_board(&self, project: Id, visible: Option<Id>, redo: bool) -> Option<Id> {
        if redo {
            if let Some(&hidden) = self.hidden_redo.get(&project) { return Some(hidden) }
        }
        visible.or_else(|| self.choice(project))
    }

    pub fn after_undo(&mut self, project: Id, board: Id) {
        if !self.boards.contains(&board) { self.hidden_redo.insert(project, board); }
    }

    pub fn refresh(&mut self, state: &State, project: Option<Id>)
        -> Option<usize> {
        use makepad_town_core::entity::{field, kind, project_entities};
        self.boards.clear();
        self.labels.clear();
        let project = project?;
        let mut boards = project_entities(state, Some(project), Some(kind::BOARD));
        boards.sort_unstable_by_key(|board| (board.created, board.id));
        for board in boards {
            self.boards.push(board.id);
            self.labels.push(board.text(field::TITLE).to_owned());
        }
        let index = self.choice(project).and_then(|id| self.boards.iter().position(|board| *board == id))
            .or_else(|| (!self.boards.is_empty()).then_some(0))?;
        self.choices.insert(project, self.boards[index]);
        Some(index)
    }
}
