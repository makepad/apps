//! The document a session holds. An edit and a deck are both a
//! Splash source with a loaded document, an undo stack and a revision; the
//! AI's source tools, the undo/redo tools, the change feed and the page's own
//! writes go through this one type, so there is one place that records a
//! change.

use makepad_asset_data::{AssetAlias, AssetRevisionRef};
use makepad_asset_data::tree::TreeDraft;
use makepad_story_deck::DeckDoc;
use makepad_story_cut::CutDoc;
use std::sync::Arc;

/// Which kind of document a session (or a tool call) is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Cut,
    Deck,
}

impl DocKind {
    /// The key the agent and the change feed know the document by.
    pub fn key(self) -> &'static str {
        match self {
            DocKind::Cut => "cut",
            DocKind::Deck => "deck",
        }
    }

    /// What the document is called in a tool's refusal.
    pub fn noun(self) -> &'static str {
        match self {
            DocKind::Cut => "cut",
            DocKind::Deck => "deck",
        }
    }
}

/// A loaded document of either kind.
#[derive(Clone, Debug)]
pub enum OpenDoc {
    Cut(Arc<CutDoc>),
    Deck(Arc<DeckDoc>),
    Tree { doc: TreeDoc, tree: Arc<TreeDraft>, path: String },
}

#[derive(Clone, Debug)]
pub enum TreeDoc {
    Cut(Arc<CutDoc>),
    Deck(Arc<DeckDoc>),
}

impl TreeDoc {
    pub fn into_cut(self) -> Result<Arc<CutDoc>, String> {
        match self { Self::Cut(doc) => Ok(doc), Self::Deck(_) => Err("the file is a deck, not a cut".into()) }
    }

    pub fn into_deck(self) -> Result<Arc<DeckDoc>, String> {
        match self { Self::Deck(doc) => Ok(doc), Self::Cut(_) => Err("the file is a cut, not a deck".into()) }
    }
}

impl OpenDoc {
    pub fn into_cut(self) -> Result<Arc<CutDoc>, String> {
        match self {
            OpenDoc::Cut(doc) => Ok(doc),
            OpenDoc::Deck(_) | OpenDoc::Tree { .. } => Err("a standalone cut was required here".into()),
        }
    }
}

/// An open document: its source, the loaded document, and the history.
pub struct Session<D = CutDoc> {
    pub source: String,
    pub doc: Arc<D>,
    /// Bumped on every change: what the viewer renders against.
    pub revision: u64,
    pub undo: Vec<Snapshot<D>>,
    pub redo: Vec<Snapshot<D>>,
    pub tree: Option<Arc<TreeDraft>>,
    pub saved_tree: Option<Arc<TreeDraft>>,
    pub saved_doc: Option<Arc<D>>,
    /// The open file can be a cut inside a tree whose root is a deck.
    pub path: String,
    pub saved_path: String,
    pub alias: Option<AssetAlias>,
    pub history_head: Option<AssetRevisionRef>,
    /// The library revision this edit was opened from or last saved as.
    pub head: Option<AssetRevisionRef>,
    /// The source as it was last opened or saved.
    pub saved: String,
}

pub struct Snapshot<D> {
    pub path: String,
    pub source: String,
    pub doc: Arc<D>,
    pub tree: Option<Arc<TreeDraft>>,
}

/// Files the app writes by itself, not the user's document.
fn is_internal(path: &str) -> bool { path.starts_with("i18n/") }

/// Two trees equal in everything the user authored.
fn same_authored(a: &TreeDraft, b: &TreeDraft) -> bool {
    let mut x = a.files.iter().filter(|(path, _)| !is_internal(path));
    let mut y = b.files.iter().filter(|(path, _)| !is_internal(path));
    a.name == b.name && a.root == b.root && a.optional == b.optional && loop {
        match (x.next(), y.next()) {
            (None, None) => break true,
            (Some((pa, fa)), Some((pb, fb))) if pa == pb && fa.kind == fb.kind && (Arc::ptr_eq(&fa.bytes, &fb.bytes) || fa.bytes == fb.bytes) => {}
            _ => break false,
        }
    }
}

impl<D> Session<D> {
    pub fn open(source: String, doc: Arc<D>, revision: u64, head: Option<AssetRevisionRef>, saved: bool) -> Self {
        Session { saved: if saved { source.clone() } else { String::new() }, saved_doc: saved.then(|| doc.clone()), source, doc, revision, undo: Vec::new(), redo: Vec::new(), head, tree: None, saved_tree: None, path: String::new(), saved_path: String::new(), alias: None, history_head: head }
    }

    pub fn dirty(&self) -> bool {
        // A tree's files say it all (the open file may be another deck of
        // the tree than the one `saved` was read from).
        // Only what the user authored counts: the translation tables the
        // output window prepares (`i18n/`) are written into the tree by the
        // app itself and must not block opening another deck.
        if let (Some(tree), Some(saved)) = (&self.tree, &self.saved_tree) { return !same_authored(tree, saved); }
        self.source != self.saved || self.tree != self.saved_tree
    }

    /// The tree differs from the saved one only in files the app writes
    /// itself: not a change to ask about, but still worth a save.
    pub fn internal_changes(&self) -> bool {
        match (&self.tree, &self.saved_tree) {
            (Some(tree), Some(saved)) => tree != saved && same_authored(tree, saved),
            _ => false,
        }
    }

    /// A change: one undo step. Returns the source and document it replaced.
    pub fn install(&mut self, source: String, doc: Arc<D>) -> (String, Arc<D>) {
        let tree = self.tree.as_ref().map(|tree| {
            let mut next = (**tree).clone();
            let path = self.path.clone();
            if let Some(file) = next.files.get(&path) {
                let kind = file.kind;
                // The existing root path has already passed path validation.
                next.write(&path, kind, source.as_bytes().into()).expect("valid root path");
            }
            Arc::new(next)
        });
        self.install_tree(source, doc, tree)
    }

    pub fn install_tree(&mut self, source: String, doc: Arc<D>, tree: Option<Arc<TreeDraft>>) -> (String, Arc<D>) {
        let before = std::mem::replace(&mut self.source, source);
        let previous = std::mem::replace(&mut self.doc, doc);
        let tree = std::mem::replace(&mut self.tree, tree);
        self.undo.push(Snapshot { path: self.path.clone(), source: before.clone(), doc: previous.clone(), tree });
        self.redo.clear();
        self.revision += 1;
        (before, previous)
    }

    /// Undo (or redo) one step; false when there is none.
    pub fn step(&mut self, redo: bool) -> bool {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(snapshot) = from.pop() else { return false };
        to.push(Snapshot { path: std::mem::replace(&mut self.path, snapshot.path), source: std::mem::replace(&mut self.source, snapshot.source), doc: std::mem::replace(&mut self.doc, snapshot.doc), tree: std::mem::replace(&mut self.tree, snapshot.tree) });
        self.revision += 1;
        true
    }

    /// A publication is one history step, including every changed file. Keep
    /// edits made after the submitted snapshot, and rebase their file origins
    /// onto the revisions just published so the next save advances those files.
    pub fn published(&mut self, submitted: &Arc<TreeDraft>, draft: Arc<TreeDraft>, path: String, doc: Arc<D>, head: AssetRevisionRef) {
        let end = if self.tree.as_ref() == Some(submitted) { self.undo.len() } else {
            self.undo.iter().rposition(|snapshot| snapshot.tree.as_ref() == Some(submitted)).unwrap_or(self.undo.len())
        };
        let newer = self.undo.split_off(end);
        self.undo.clear();
        if let (Some(previous), Some(doc)) = (&self.saved_tree, &self.saved_doc) {
            if previous != submitted {
                let path = self.saved_path.clone();
                self.undo.push(Snapshot { path, source: self.saved.clone(), doc: doc.clone(), tree: Some(previous.clone()) });
            }
        }
        self.undo.extend(newer);
        self.history_head = self.head;
        self.head = Some(head);
        self.saved = draft.text(&path).expect("published document path was validated").into();
        self.saved_path = path;
        self.saved_tree = Some(draft.clone());
        self.saved_doc = Some(doc.clone());
        if self.tree.as_ref() == Some(submitted) {
            self.tree = Some(draft);
            self.doc = doc;
        } else {
            rebase(&mut self.tree, submitted, &draft);
        }
        for snapshot in self.undo.iter_mut().chain(&mut self.redo) { rebase(&mut snapshot.tree, submitted, self.saved_tree.as_ref().unwrap()); }
        self.revision += 1;
    }

    pub fn restore_previous(&mut self, tree: Arc<TreeDraft>, path: String, doc: Arc<D>, head: AssetRevisionRef) -> Result<(), String> {
        let source = tree.text(&path)?.to_string();
        self.redo.push(Snapshot { path: std::mem::replace(&mut self.path, path), source: std::mem::replace(&mut self.source, source), doc: std::mem::replace(&mut self.doc, doc), tree: self.tree.replace(tree) });
        self.history_head = Some(head);
        self.revision += 1;
        Ok(())
    }
}

fn rebase(tree: &mut Option<Arc<TreeDraft>>, submitted: &TreeDraft, saved: &TreeDraft) {
    let Some(current) = tree else { return; };
    let mut next = (**current).clone();
    for (path, file) in &mut next.files {
        if let Some(published) = saved.files.get(path) {
            if file.base == submitted.files.get(path).and_then(|file| file.base) {
                file.base = published.reference;
                if file.bytes == published.bytes { file.reference = published.reference; }
            }
        }
    }
    *current = Arc::new(next);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_writes_leave_a_tree_session_clean_and_a_real_edit_makes_it_dirty() {
        use makepad_asset_data::{tree::TreeRole, AssetKind};
        let tree = Arc::new(TreeDraft::new("talk".into(), TreeRole::Deck, "deck.splash".into(), AssetKind::Deck, "a".into()).unwrap());
        let mut session: Session<String> = Session::open("a".into(), Arc::new("A".into()), 1, None, true);
        session.tree = Some(tree.clone());
        session.saved_tree = Some(tree.clone());
        assert!(!session.dirty() && !session.internal_changes());
        // The output window's translation table: written by the app.
        let mut table = (*tree).clone();
        table.write("i18n/zh.json", AssetKind::Deck, b"{}".as_slice().into()).unwrap();
        session.tree = Some(Arc::new(table.clone()));
        assert!(!session.dirty(), "a translation table is not the user's change");
        assert!(session.internal_changes(), "but it is still worth saving");
        // A real edit.
        table.write("deck.splash", AssetKind::Deck, b"b".as_slice().into()).unwrap();
        session.tree = Some(Arc::new(table));
        assert!(session.dirty());
    }

    #[test]
    fn a_session_of_either_kind_keeps_one_undo_stack() {
        let mut session: Session<String> = Session::open("a".into(), Arc::new("A".into()), 1, None, true);
        assert!(!session.dirty());
        let (before, previous) = session.install("b".into(), Arc::new("B".into()));
        assert_eq!((before.as_str(), previous.as_str(), session.revision), ("a", "A", 2));
        assert!(session.dirty());
        session.install("c".into(), Arc::new("C".into()));
        assert!(session.step(false) && session.source == "b" && *session.doc == "B");
        assert!(session.step(false) && session.source == "a");
        assert!(!session.step(false), "nothing left to undo");
        assert!(session.step(true) && session.source == "b");
        // A new change drops the redo stack.
        session.install("d".into(), Arc::new("D".into()));
        assert!(!session.step(true));
        assert_eq!(session.revision, 7);
        assert_eq!(DocKind::Deck.key(), "deck");
        assert_eq!(DocKind::Cut.key(), "cut");
    }
}
