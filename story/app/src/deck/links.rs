//! Several decks in one tree: the switcher, link slides and the way back.
//!
//! A tree's root is `deck.splash`; any other `<name>.deck.splash` is a deck
//! too (a tree of talks). The page shows one of them at a time: the session's
//! `path` is that deck file, so edits, Present, the strip and the stills all
//! follow it. A slide with `link: "deck.splash"` hands Next to the page past
//! its last beat: the page enters the linked deck through its transition and
//! remembers where it came from; Backspace (or Next past the linked deck's
//! last slide) goes back to the slide after the link. Decks load on a worker
//! thread (a slide's graphic can compute for seconds); the linked decks of
//! the open one are loaded ahead so the jump is immediate.
use super::*;
use super::player::LinkRequest;
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver};

/// Where a link came from: the deck to go back to and the slide to show.
#[derive(Clone, Debug)]
pub(super) struct Return {
    pub path: String,
    pub title: String,
    pub slide: usize,
}

enum Load {
    Loading(Receiver<Result<Arc<DeckDoc>, String>>),
    Ready(Arc<DeckDoc>),
    Failed(String),
}

/// The tree's decks, loaded in the background for one tree snapshot.
#[derive(Default)]
pub(super) struct DeckLoads {
    tree: Option<Arc<TreeDraft>>,
    loads: HashMap<String, Load>,
}

impl DeckLoads {
    fn request(&mut self, tree: &Arc<TreeDraft>, path: &str) {
        if !self.tree.as_ref().is_some_and(|known| Arc::ptr_eq(known, tree)) {
            self.tree = Some(tree.clone());
            self.loads.clear();
        }
        if self.loads.contains_key(path) { return; }
        let (send, receive) = channel();
        let (tree, owned) = (tree.clone(), path.to_string());
        let spawned = std::thread::Builder::new().name("stage-deck-load".into()).spawn(move || {
            let _ = send.send(super::super::document_tree::load_deck_at(&tree, &owned));
        });
        self.loads.insert(path.into(), match spawned { Ok(_) => Load::Loading(receive), Err(error) => Load::Failed(error.to_string()) });
    }

    /// Finished loads; true when one finished.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        for load in self.loads.values_mut() {
            let Load::Loading(receive) = load else { continue };
            match receive.try_recv() {
                Ok(Ok(doc)) => { *load = Load::Ready(doc); changed = true; }
                Ok(Err(error)) => { *load = Load::Failed(error); changed = true; }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { *load = Load::Failed("the deck's loader stopped".into()); changed = true; }
            }
        }
        changed
    }

    /// The loaded deck `path` of `tree`; None while it loads (or not asked).
    fn get(&self, tree: &Arc<TreeDraft>, path: &str) -> Option<Result<Arc<DeckDoc>, String>> {
        if !self.tree.as_ref().is_some_and(|known| Arc::ptr_eq(known, tree)) { return None; }
        match self.loads.get(path)? {
            Load::Loading(_) => None,
            Load::Ready(doc) => Some(Ok(doc.clone())),
            Load::Failed(error) => Some(Err(error.clone())),
        }
    }
}

/// A switch waiting for its deck to load.
#[derive(Clone, Debug)]
pub(super) struct PendingSwitch {
    path: String,
    /// The slide to enter (by name, resolved in the loaded deck), none: 0.
    slide: Option<String>,
    /// Or by index (the way back).
    index: Option<usize>,
    /// Pushed on the trail once the switch happens (a link's way back).
    back: Option<Return>,
    /// A transition from the picture shown now (a link or its return); a
    /// pick in the switcher opens the deck plainly.
    animate: bool,
}

impl DeckPage {
    fn tree(&self) -> Option<Arc<TreeDraft>> {
        self.session.as_ref().and_then(|session| session.tree.clone())
    }

    /// Load the linked decks of the open deck ahead, so a jump does not
    /// wait. Asked again only when the open deck changes.
    pub(super) fn preload_links(&mut self) {
        let open = self.session.as_ref().map(|session| (session.path.clone(), session.head));
        if self.switch_seen == open { return; }
        self.switch_seen = open;
        if let (Some(tree), Some(doc)) = (self.tree(), self.doc.clone()) {
            for link in doc.slides.iter().filter_map(|slide| slide.link.as_ref()) { self.deck_loads.request(&tree, &link.path); }
        }
    }

    /// Show deck file `path` of the open tree from its start (a Library card
    /// of another talk in the same folder).
    pub(super) fn switch_deck(&mut self, cx: &mut Cx, ui: &WidgetRef, path: String) {
        self.trail.clear();
        self.switch_to(cx, ui, PendingSwitch { path, slide: None, index: None, back: None, animate: false });
    }

    /// Backspace: back to where the last link came from.
    pub(super) fn go_back(&mut self, cx: &mut Cx, ui: &WidgetRef) -> bool {
        let Some(back) = self.trail.pop() else { return false };
        self.switch_to(cx, ui, PendingSwitch { path: back.path, slide: None, index: Some(back.slide), back: None, animate: true });
        true
    }

    /// Every pump: what Next asked (a link, the deck's end), and decks
    /// that finished loading.
    pub(super) fn pump_links(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let request = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>().and_then(|mut view| view.take_link());
        match request {
            Some(LinkRequest::Follow(link)) => {
                let back = self.session.as_ref().zip(self.doc.as_ref()).map(|(session, doc)| {
                    let current = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()).unwrap_or(0);
                    Return { path: session.path.clone(), title: doc.title.clone(), slide: (current + 1).min(doc.slides.len().saturating_sub(1)) }
                });
                self.switch_to(cx, ui, PendingSwitch { path: link.path, slide: link.slide, index: None, back, animate: true });
            }
            Some(LinkRequest::End) => { self.go_back(cx, ui); }
            None => {}
        }
        if self.deck_loads.poll() {
            if let Some(pending) = self.pending_switch.take() { self.switch_to(cx, ui, pending); }
        }
    }

    fn switch_to(&mut self, cx: &mut Cx, ui: &WidgetRef, pending: PendingSwitch) {
        let Some(tree) = self.tree() else { return };
        if self.editing || !self.pending.is_empty() || self.source.as_ref().is_some_and(SourcePane::has_draft) || self.file_sources.values().any(SourcePane::has_draft) {
            self.bar_notice = "Another deck opens once the edit in progress is done".into();
            return;
        }
        let loaded = if self.session.as_ref().is_some_and(|session| session.path == pending.path) {
            self.session.as_ref().map(|session| Ok(session.doc.clone()))
        } else {
            self.deck_loads.request(&tree, &pending.path);
            self.deck_loads.get(&tree, &pending.path)
        };
        let doc = match loaded {
            None => {
                self.bar_notice = format!("Opening {}…", pending.path);
                self.pending_switch = Some(pending);
                cx.redraw_all();
                return;
            }
            Some(Err(error)) => {
                self.bar_notice.clear();
                self.load_error = format!("{}: {error}", pending.path);
                return;
            }
            Some(Ok(doc)) => doc,
        };
        self.bar_notice.clear();
        let to = match (&pending.slide, pending.index) {
            (_, Some(index)) => index.min(doc.slides.len().saturating_sub(1)),
            (Some(name), None) => doc.slides.iter().position(|slide| slide.name == *name).unwrap_or(0),
            (None, None) => 0,
        };
        let Ok(source) = tree.text(&pending.path).map(str::to_string) else { return };
        if let Some(session) = &mut self.session {
            session.path = pending.path.clone();
            session.source = source;
            session.doc = doc.clone();
            session.revision += 1;
        }
        if let Some(back) = pending.back { self.trail.push(back); }
        // The panes follow the deck shown now.
        self.active_file.clear();
        self.file_slide = None;
        self.element = None;
        self.structure_seen = None;
        self.source_seen = None;
        self.revealed = None;
        self.source = None;
        self.files_seen = None;
        self.header_seen = None;
        self.synced = None;
        let shown = Rc::new((*doc).clone());
        self.doc = Some(shown.clone());
        if let Some(mut view) = ui.widget(cx, ids!(deck_view)).borrow_mut::<DeckView>() {
            if pending.animate {
                let transition = doc.slides.get(to).and_then(|slide| slide.enter.clone()).unwrap_or_else(|| doc.transition.clone());
                view.jump(cx, shown, to, transition);
            } else {
                view.set_doc(cx, Some(shown));
                if to > 0 { view.command(cx, Command::Jump(to)); }
            }
            let thumbnails = view.thumbnails();
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.bind(cx, thumbnails); }
        }
        log!("deck: showing {} ({}) at slide {to}, way back: {}", pending.path, doc.title, self.trail.last().map_or("none".to_string(), |back| format!("{} slide {}", back.path, back.slide + 1)));
        cx.redraw_all();
    }

    /// The presenter's line for the way back, empty when there is none.
    pub(super) fn return_line(&self) -> String {
        // Short: it shares the bar with the clock (Backspace goes back).
        self.trail.last().map_or_else(String::new, |back| {
            let title: String = if back.title.chars().count() > 24 { back.title.chars().take(23).chain(std::iter::once('…')).collect() } else { back.title.clone() };
            format!("return: {title} · slide {}", back.slide + 1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_followed_link_leaves_a_way_back_that_needs_no_switcher() {
        let mut page = DeckPage::default();
        assert_eq!(page.return_line(), "");
        // What `pump_links` pushes (through `switch_to`) when a link is followed.
        page.trail.push(Return { path: "decks/a.deck.splash".into(), title: "A talk with a very long title indeed".into(), slide: 6 });
        assert_eq!(page.return_line(), "return: A talk with a very long… · slide 7");
        // Backspace pops it; with nothing left there is no way back.
        let back = page.trail.pop().expect("a way back");
        assert_eq!((back.path.as_str(), back.slide), ("decks/a.deck.splash", 6));
        assert!(page.trail.is_empty());
        assert_eq!(page.return_line(), "");
    }
}
