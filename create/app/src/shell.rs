//! The shell the person sees: the town tree on the left, the town's Home and a project's
//! overview in the middle, and each item's page under its breadcrumb and title.
//! Every row, tile, crumb and action here carries an id; titles are display text only.

use super::*;
use makepad_create::lists::{home_grid, project_grid, summary, Delete, GridRow, NewKind, Node, Press, Target};
use makepad_create::model::tree::RowKind;

/// The page's title field, which one rename writes.
struct Title {
    id: Id,
    shown: String,
    editable: bool,
}

impl App {
    /// The tree row of the page that is open.
    fn current_node(&self) -> Option<Node> {
        let town = self.view.tree.snapshot().town?;
        let project = self.view.workspace.current;
        match self.view.page {
            Page::Home => Some(Node::Town(town)),
            Page::Live(item) => Some(Node::Live(item)),
            Page::Project => project.map(Node::Project),
            Page::Tasks => project.map(Node::Tasks),
            Page::Issues => project.map(Node::Issues),
            Page::Board => self.view.open_board.map(Node::Board),
            Page::Docs => self.view.open_doc.map(Node::Doc),
            Page::Publish | Page::Join => None,
        }
    }

    /// A page whose project, board or doc is gone (a peer removed it) falls back to the
    /// project's overview, or to Home.
    fn validate_page(&mut self) {
        let tree = self.view.tree.snapshot();
        let project = self.view.workspace.current.filter(|p| tree.project(*p).is_some());
        let exists = |id: Option<Id>| id.is_some_and(|id| tree.row(id).is_some());
        let valid = match self.view.page {
            Page::Project | Page::Tasks | Page::Issues => project.is_some(),
            Page::Board => exists(self.view.open_board),
            Page::Docs => exists(self.view.open_doc),
            _ => true,
        };
        if !valid { self.view.page = if project.is_some() { Page::Project } else { Page::Home }; }
        // The Join page lasts while its words do; then the open town's Home is back.
        if self.view.page == Page::Join && !self.view.connect.shows_pairing(makepad_create::model::PairSide::Join) { self.view.page = Page::Home; }
    }

    /// The previous project or document may have been removed while the call page was open.
    pub(super) fn return_from_call(&mut self) {
        let (page, node, pane) = self.before_call.take().unwrap_or((Page::Home, None, Pane::Page));
        if self.view.narrow() && self.view.pane == Pane::Page { self.view.show_pane(pane); }
        self.view.page = page;
        self.validate_page();
        if self.view.page != page || self.current_node() != node
            || matches!(page, Page::Live(item) if !self.view.has_live(item)) {
            self.view.page = Page::Home;
        }
    }

    /// Opens the page of a tree row, tile or crumb by its id; an id that isn't there is ignored.
    pub(super) fn open_node(&mut self, cx: &mut Cx, node: Node) {
        if self.flush_doc(cx).is_err() { return }
        let tree = self.view.tree.snapshot();
        match node {
            Node::Town(town) => {
                if Some(town) != tree.town { self.switch_town(cx, town); }
                if self.view.tree.snapshot().town != Some(town) { return }
                self.view.page = Page::Home;
            }
            Node::Live(item) => {
                if !self.view.has_live(item) { return }
                let next = Page::Live(item);
                if item == Live::Call && self.view.page != next {
                    let pane = if self.view.narrow() { self.view.pane } else { Pane::Page };
                    self.before_call = Some((self.view.page, self.current_node(), pane));
                }
                self.view.page = next;
            }
            Node::Project(project) | Node::Tasks(project) | Node::Issues(project) => {
                if tree.project(project).is_none() { return }
                if matches!(node, Node::Issues(_)) {
                    if let Some(model) = &mut self.model { self.view.issues.show_index(cx, model); }
                }
                self.select_project(project);
                self.view.page = match node { Node::Tasks(_) => Page::Tasks, Node::Issues(_) => Page::Issues, _ => Page::Project };
            }
            Node::Board(board) | Node::Doc(board) => {
                let Some(project) = tree.row(board).and_then(|_| tree.project_of(board)).map(|p| p.id) else { return };
                self.select_project(project);
                if matches!(node, Node::Board(_)) {
                    self.board_picker.choose(project, board);
                    self.view.open_board = Some(board);
                    self.view.page = Page::Board;
                    self.select_board(cx);
                } else {
                    self.view.open_doc = Some(board);
                    self.view.page = Page::Docs;
                }
            }
        }
        self.view.show_pane(Pane::Page);
        self.update_page(cx);
    }

    /// Opens a page of the current project (a Narrow window then shows it).
    pub(super) fn open_page(&mut self, cx: &mut Cx, page: Page) {
        let tree = self.view.tree.snapshot();
        let project = self.view.workspace.current.and_then(|id| tree.project(id));
        let first = |kind| project.and_then(|p| p.rows.iter().find(|r| r.kind == kind)).map(|r| r.id);
        let node = match page {
            Page::Home => tree.town.map(Node::Town),
            Page::Tasks => project.map(|p| Node::Tasks(p.id)),
            Page::Issues => project.map(|p| Node::Issues(p.id)),
            Page::Board => self.view.open_board.filter(|b| project.is_some_and(|p| p.rows.iter().any(|r| r.id == *b)))
                .or_else(|| first(RowKind::Board)).map(Node::Board).or(project.map(|p| Node::Project(p.id))),
            Page::Docs => self.view.open_doc.filter(|d| project.is_some_and(|p| p.rows.iter().any(|r| r.id == *d)))
                .or_else(|| first(RowKind::Doc)).map(Node::Doc).or(project.map(|p| Node::Project(p.id))),
            _ => None,
        };
        match node {
            Some(node) => self.open_node(cx, node),
            None => {
                if self.flush_doc(cx).is_ok() { self.view.page = page; self.view.show_pane(Pane::Page); self.update_page(cx); }
            }
        }
    }

    /// The name the narrow bar shows for the open page.
    pub(super) fn page_name(&self) -> String {
        self.title().map(|t| t.shown).filter(|name| !name.trim().is_empty()).unwrap_or_else(|| match self.view.page {
            Page::Home => "Home", Page::Project => "Project", Page::Tasks => "Tasks", Page::Issues => "Issues",
            Page::Board => "Board", Page::Docs => "Docs",
            Page::Live(Live::Call) => "Call", Page::Live(Live::Share(_)) => "Live share", Page::Publish => "Publish", Page::Join => "Join a town",
        }.into())
    }

    /// The breadcrumb of the open page: the town, then the project, each by its id.
    fn crumbs(&self) -> Vec<(Node, String)> {
        let tree = self.view.tree.snapshot();
        let Some(town) = tree.town else { return Vec::new() };
        let mut crumbs = vec![(Node::Town(town), tree.label.clone())];
        if matches!(self.view.page, Page::Tasks | Page::Issues | Page::Board | Page::Docs) {
            if let Some(project) = self.view.workspace.current.and_then(|id| tree.project(id)) {
                crumbs.push((Node::Project(project.id), project.title.clone()));
            }
        }
        crumbs
    }

    fn title(&self) -> Option<Title> {
        let tree = self.view.tree.snapshot();
        let title = |id: Id, shown: &str, editable| Some(Title { id, shown: shown.to_owned(), editable });
        let project = self.view.workspace.current.and_then(|id| tree.project(id));
        match self.view.page {
            Page::Home => title(tree.town?, &tree.label, true),
            Page::Project => project.and_then(|p| title(p.id, &p.title, true)),
            Page::Tasks => project.and_then(|p| title(p.id, "Tasks", false)),
            Page::Issues => project.and_then(|p| title(p.id, "Issues", false)),
            Page::Board => self.view.open_board.and_then(|b| tree.row(b)).and_then(|r| title(r.id, &r.title, true)),
            Page::Docs => self.view.open_doc.and_then(|d| tree.row(d)).and_then(|r| title(r.id, &r.title, true)),
            _ => None,
        }
    }

    /// Rebuilds what the shell's pages show. Runs with each page update: it walks the open
    /// town's projects and rows once, so it grows with their count and nothing else.
    pub(super) fn show_shell(&mut self, cx: &mut Cx) {
        self.validate_page();
        self.view.tree.show(self.current_node());
        let columns = if self.view.narrow() { 1 } else { 3 };
        let page = self.view.page;
        let tree = self.view.tree.snapshot();
        if page != Page::Home || self.title_shown.as_ref().map(|t| t.0) != tree.town {
            let area = self.ui.text_input(cx, ids!(home_title)).area();
            if !area.is_empty() && cx.has_key_focus(area) { cx.set_key_focus(Area::Empty); }
        }
        self.view.home_grid = home_grid(tree, columns);
        self.view.overview_grid = self.view.workspace.current.map(|p| project_grid(tree, p, columns)).unwrap_or_default();
        let project = self.view.workspace.current.and_then(|id| tree.project(id));
        let project_meta = project.map(|p| {
            let count = |k| p.rows.iter().filter(|r| r.kind == k).count();
            summary(p.tasks, count(RowKind::Board), count(RowKind::Doc))
        }).unwrap_or_default();
        let crumbs = self.crumbs();
        let title = self.title();
        let item = matches!(page, Page::Tasks | Page::Issues | Page::Board | Page::Docs);
        let writable = self.view.workspace.writable;
        let removable = self.view.workspace.removable.is_some_and(|r| Some(r) == self.view.workspace.current) && writable;
        let removed = self.view.workspace.removed.as_ref().map(|r| r.title.clone());

        for (path, visible) in [
            (ids!(home_page), page == Page::Home), (ids!(project_page), page == Page::Project),
            (ids!(item_head), item),
        ] {
            self.ui.widget(cx, path).set_visible(cx, visible);
        }
        self.ui.label(cx, ids!(project_meta)).set_text(cx, &project_meta);
        let note = self.ui.widget(cx, ids!(home_removed));
        note.set_visible(cx, removed.is_some());
        if let Some(removed) = &removed { note.label(cx, ids!(removed_title)).set_text(cx, removed); }
        self.ui.widget(cx, ids!(remove_project)).set_visible(cx, removable);

        // Titles: draw the model's, except while the person is typing in the field.
        let input_path = match page {
            Page::Home => Some(ids!(home_title)),
            Page::Project => Some(ids!(project_title)),
            Page::Tasks | Page::Issues | Page::Board | Page::Docs => Some(ids!(item_title)),
            _ => None,
        };
        if let (Some(path), Some(title)) = (input_path, &title) {
            let input = self.ui.text_input(cx, path);
            if self.title_shown.as_ref().map(|t| (t.0, &t.1)) != Some((title.id, &title.shown)) {
                input.set_text(cx, &title.shown);
                self.title_shown = Some((title.id, title.shown.clone()));
            }
            input.set_is_read_only(cx, !(title.editable && writable));
        } else {
            self.title_shown = None;
        }

        // The crumbs and the ⋯ menu.
        let town = crumbs.first();
        for (path, crumb) in [(ids!(crumb_town), town), (ids!(project_crumb), town), (ids!(crumb_project), crumbs.get(1))] {
            let button = self.ui.button(cx, path);
            button.set_visible(cx, crumb.is_some());
            if let Some((_, label)) = crumb { button.set_text(cx, label); }
        }
        let deletable = matches!(page, Page::Board | Page::Docs) && writable;
        let target = title.as_ref().filter(|_| deletable);
        if !target.is_some_and(|t| matches!(self.view.delete, Delete::Menu(id) | Delete::Confirm(id) if id == t.id)) {
            self.view.delete = Delete::Closed;
        }
        self.ui.widget(cx, ids!(item_dots)).set_visible(cx, target.is_some());
        self.ui.widget(cx, ids!(delete_menu)).set_visible(cx, matches!(self.view.delete, Delete::Menu(_)));
        self.ui.widget(cx, ids!(delete_sheet)).set_visible(cx, matches!(self.view.delete, Delete::Confirm(_)));
        if let Some(t) = target {
            let name = &t.shown;
            for (path, text) in [
                (ids!(delete_ask), format!("Delete {name}…")), (ids!(delete_confirm), format!("Delete {name}")),
                (ids!(delete_keep), format!("Keep {name}")),
            ] {
                self.ui.button(cx, path).set_text(cx, &text);
            }
            self.ui.label(cx, ids!(delete_question)).set_text(cx, &format!("Delete {name}?"));
        }
    }

    /// A new project, board or doc opens at once; its row appears in the tree.
    fn add_to_town(&mut self, cx: &mut Cx, kind: NewKind) {
        let Some(project) = self.view.workspace.current else { return };
        let (action, open): (Action, fn(Id) -> Node) = match kind {
            NewKind::Project => (Action::AddProject { title: "New project".into() }, Node::Project),
            NewKind::Board => (Action::AddBoard { title: "New board".into(), project }, Node::Board),
            NewKind::Doc => (Action::AddDoc { title: "New doc".into(), project }, Node::Doc),
        };
        if self.flush_doc(cx).is_err() { return }
        let Some(id) = self.act(cx, action) else { return };
        if kind == NewKind::Board { self.board_history.entry(id).or_default().record(id); }
        self.open_node(cx, open(id));
    }

    /// The field's text was committed (Return or leaving) or abandoned (Escape). Writes a
    /// changed title by the shown id; returns the text the field should show afterwards: the
    /// old title for Escape, an empty title or a refusal.
    fn edit_title(&mut self, cx: &mut Cx, text: &str, escaped: bool) -> Option<String> {
        let (id, shown) = self.title_shown.clone()?;
        let title = text.trim();
        if escaped || title.is_empty() { return Some(shown) }
        if title == shown { return None }
        if Some(id) == self.view.tree.snapshot().town {
            if let Err(e) = self.model.as_mut()?.rename_town(title) { log!("town: {e}"); return Some(shown) }
            self.deliver();
            self.refresh(cx);
            return None;
        }
        let action = match self.model.as_ref()?.rename_action(id, title) {
            Ok(Some(action)) => action,
            Ok(None) => return None,
            Err(e) => { log!("town: {e}"); return Some(shown) }
        };
        let board = matches!(self.view.page, Page::Board);
        if let Some(event) = self.act(cx, action) {
            if board { self.board_history.entry(id).or_default().record(event); }
        }
        None
    }

    fn commit_title(&mut self, cx: &mut Cx, path: &[LiveId], escaped: bool) {
        let input = self.ui.text_input(cx, path);
        if let Some(shown) = self.edit_title(cx, &input.text(), escaped) { input.set_text(cx, &shown); }
    }

    pub(super) fn shell_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        // The tree: a twisty folds its own row; a name opens its id.
        let mut opens: Vec<_> = actions.iter().filter_map(|a| a.as_widget_action()).filter_map(Node::clicked).collect();
        let tree = self.ui.portal_list(cx, ids!(tree.list));
        for (index, row) in tree.items_with_actions(actions) {
            if row.button(cx, ids!(twisty)).clicked(actions) {
                opens.extend(self.view.tree.press(index, Press::Twisty));
                self.ui.redraw(cx);
            }
        }
        // Tiles.
        for (path, overview) in [(ids!(home_grid.list), false), (ids!(overview_grid.list), true)] {
            let grid = if overview { &self.view.overview_grid } else { &self.view.home_grid };
            let list = self.ui.portal_list(cx, path);
            for (index, row) in list.items_with_actions(actions) {
                let Some(GridRow::Tiles(tiles)) = grid.get(index) else { continue };
                for (slot, tile) in [ids!(tile_a.open), ids!(tile_b.open), ids!(tile_c.open)].into_iter().enumerate() {
                    if row.button(cx, tile).clicked(actions) {
                        if let Some(tile) = tiles.get(slot) {
                            match tile.target {
                                Target::Open(node) => opens.push(node),
                                Target::New(kind) => { self.add_to_town(cx, kind); return }
                            }
                        }
                    }
                }
            }
        }
        // Crumbs, each by its id.
        let crumbs = self.crumbs();
        for (path, i) in [(ids!(crumb_town), 0), (ids!(project_crumb), 0), (ids!(crumb_project), 1)] {
            if self.ui.button(cx, path).clicked(actions) { opens.extend(crumbs.get(i).map(|c| c.0)); }
        }
        for node in opens { self.open_node(cx, node); }

        // Rename in place: Return or leaving the field writes; Escape puts the old title back.
        let title_field = match self.view.page {
            Page::Home => Some(ids!(home_title)),
            Page::Project => Some(ids!(project_title)),
            Page::Board | Page::Docs => Some(ids!(item_title)),
            _ => None,
        };
        if let Some(path) = title_field {
            let input = self.ui.text_input(cx, path);
            let escaped = input.escaped(actions);
            if escaped || input.returned(actions).is_some() || input.key_focus_lost(actions) { self.commit_title(cx, path, escaped); }
        }

        // Delete from ⋯, with its one confirmation.
        let item = self.title_shown.as_ref().map(|t| t.0);
        if let (Some(id), true) = (item, matches!(self.view.page, Page::Board | Page::Docs)) {
            if self.ui.button(cx, ids!(item_dots)).clicked(actions) {
                self.view.delete = if matches!(self.view.delete, Delete::Closed) { Delete::Menu(id) } else { Delete::Closed };
                self.update_page(cx);
            } else if self.ui.button(cx, ids!(delete_ask)).clicked(actions) {
                self.view.delete = Delete::Confirm(id);
                self.update_page(cx);
            } else if self.ui.button(cx, ids!(delete_keep)).clicked(actions) {
                self.view.delete = Delete::Closed;
                self.update_page(cx);
            } else if self.ui.button(cx, ids!(delete_confirm)).clicked(actions) && self.view.delete == Delete::Confirm(id) {
                self.delete_page(cx, id);
            }
        }

        // Projects keep their remove with undo.
        if self.ui.button(cx, ids!(undo_project)).clicked(actions) {
            self.undo_removed_project(cx);
        } else if self.ui.button(cx, ids!(remove_project)).clicked(actions) {
            self.remove_project(cx);
        }
    }

    fn delete_page(&mut self, cx: &mut Cx, id: Id) {
        let Some(project) = self.view.tree.snapshot().project_of(id).map(|p| p.id) else { return };
        let Some(model) = &self.model else { return };
        let Ok(action) = model.delete_page_action(id) else { return };
        if self.flush_doc(cx).is_err() { return }
        self.view.delete = Delete::Closed;
        if self.act(cx, action).is_some() { self.open_node(cx, Node::Project(project)); }
    }

    #[cfg(test)]
    fn delete_page_refused(&self, id: Id) -> bool {
        self.model.as_ref().is_some_and(|m| m.delete_page_action(id).is_err())
    }

    fn remove_project(&mut self, cx: &mut Cx) {
        let Some(project) = self.view.workspace.removable.filter(|p| Some(*p) == self.view.workspace.current) else { return };
        let Some(title) = self.view.workspace.selected().map(|p| p.name.clone()) else { return };
        if let Some(event) = self.act(cx, Action::RemoveProject { project }) {
            self.view.workspace.removed = Some(RemovedProject { project, event, title });
            if let Some(town) = self.view.tree.snapshot().town { self.open_node(cx, Node::Town(town)); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(label: &str) -> (Cx, App, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("create-shell-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (model, _) = Model::open(&dir).unwrap();
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut app = cx.with_vm(App::script_new);
        app.model = Some(model);
        app.refresh(&mut cx);
        (cx, app, dir)
    }

    fn titles(app: &App, id: Id) -> Vec<String> {
        let tree = app.view.tree.snapshot();
        let mut all: Vec<String> = tree.row(id).map(|r| r.title.clone()).into_iter().chain(tree.project(id).map(|p| p.title.clone())).collect();
        all.extend(app.view.tree.lines.iter().filter(|l| matches!(l.node, Node::Board(i) | Node::Doc(i) | Node::Project(i) if i == id)).map(|l| l.label.clone()));
        all.extend(app.crumbs().into_iter().filter(|(n, _)| matches!(n, Node::Project(i) if *i == id)).map(|c| c.1));
        for row in app.view.home_grid.iter().chain(&app.view.overview_grid) {
            if let GridRow::Tiles(tiles) = row {
                all.extend(tiles.iter().filter(|t| matches!(t.target, Target::Open(Node::Board(i) | Node::Doc(i) | Node::Project(i)) if i == id)).map(|t| t.title.clone()));
            }
        }
        all
    }

    #[test]
    fn home_to_a_project_to_a_new_board_and_back_by_the_crumb() {
        let (mut cx, mut app, dir) = app("pages");
        let cx = &mut cx;
        assert_eq!(app.view.page, Page::Home, "the app opens on the town's Home");
        let town = app.view.tree.snapshot().town.unwrap();
        let tile = app.view.home_grid.iter().find_map(|row| match row {
            GridRow::Tiles(tiles) => tiles.iter().find_map(|t| match t.target { Target::Open(Node::Project(p)) => Some(p), _ => None }),
            _ => None,
        }).expect("Home tiles each project");
        assert!(app.view.home_grid.iter().any(|r| matches!(r, GridRow::Tiles(t) if t.iter().any(|t| t.target == Target::New(NewKind::Project)))),
            "Home ends with a New project tile");
        app.open_node(cx, Node::Project(tile));
        assert_eq!((app.view.page, app.view.workspace.current), (Page::Project, Some(tile)), "a project tile opens its overview");
        app.add_to_town(cx, NewKind::Board);
        let board = app.view.open_board.expect("the new board opens at once");
        assert_eq!(app.view.page, Page::Board, "the new board's page is open");
        let project = app.view.tree.snapshot().project(tile).unwrap();
        assert!(project.rows.iter().any(|r| r.id == board && r.kind == RowKind::Board), "its row is under that project");
        let lines = &app.view.tree.lines;
        let at = lines.iter().position(|l| l.node == Node::Project(tile)).unwrap();
        assert!(lines[at + 1..].iter().take_while(|l| l.depth > 1).any(|l| l.node == Node::Board(board) && l.open),
            "the tree shows the board under its project, marked open");
        let crumbs = app.crumbs();
        assert_eq!(crumbs.iter().map(|c| c.0).collect::<Vec<_>>(), [Node::Town(town), Node::Project(tile)], "the crumbs carry ids");
        app.open_node(cx, crumbs[0].0);
        assert_eq!(app.view.page, Page::Home, "the breadcrumb's town part opens Home");
        app.open_node(cx, Node::Board([9; 32]));
        assert_eq!(app.view.page, Page::Home, "an unknown id opens nothing");
        app.open_page(cx, Page::Board);
        assert_eq!((app.view.page, app.view.open_board), (Page::Board, Some(board)), "a page opens the project's board");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn named_tile_and_rail_buttons_open_their_own_pages() {
        use makepad_widgets::cx_draw::CxDraw;
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            makepad_create::script_mod(vm);
        });
        let cx = &mut *pool;
        let mut app = cx.with_vm(App::script_new);
        app.ui = cx.with_vm(|vm| {
            let root = script_eval!(vm, {use mod.prelude.widgets.* use mod.widgets.* Root{
                main_window := Window{body +: {test_shell := View{
                    flow: Right compact_call := Button{visible: false} tree := TownTreeRail{width: 180} home_page := HomePage{} project_page := ProjectPage{} live_call := LiveCallPage{visible: false} live_share := LiveSharePage{visible: false}
                    issues_page := IssuesPage{visible: false}
                    chat_edge := ShareChatEdge{visible: false}
                }}}
                share_popout := Window{create_on_start: false show_caption_bar: false chrome: WindowChrome.Custom body +: {share_view := SharePopoutBody{}}}
            }});
            WidgetRef::script_from_value(vm, root)
        });
        let dir = std::env::temp_dir().join(format!("town-named-buttons-{}", std::process::id()));
        app.model = Some(Model::open(&dir).unwrap().0);
        app.refresh(cx);
        use makepad_call::driver::{std_spawn, VoiceSource};
        use makepad_create::call::{CallEnv, Message};
        app.call.configure(CallEnv { files: Default::default(), signal: std::sync::Arc::new(|| {}), voice: VoiceSource::Synthetic { tone_hz: 700. },
            devices_path: dir.join("devices"), spawn: std_spawn(), jobs: std_spawn() });
        let port = app.call.link_up(cx, [1; 65], [2; 65], "Ada").unwrap();
        let session = [5; 16];
        port.messages_in.try_send(Message::Share { session, title: "Drawing".into(), w: 1600., h: 1000., ext: Default::default() }.encode()).unwrap();
        app.call.poll(cx);
        app.show_call(cx);
        let size = dvec2(1050., 640.);
        let pass = DrawPass::new(cx);
        pass.set_size(cx, size);
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent::default();
        cx.a11y.on = true;
        let mut accessible = a11y_tree::mirror::Tree::new();
        let mut draw_buttons = |app: &mut App, cx: &mut Cx| {
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            frame.a11y.begin_frame();
            let root = frame.a11y.begin_node(WidgetUid::new().0, Some(list.id()));
            frame.a11y.begin_window(WindowId(0, 1));
            app.ui.widget(&frame, ids!(test_shell)).draw_all(&mut frame, &mut Scope::with_data(&mut app.view));
            frame.a11y.end_window();
            frame.a11y.end_node(root, a11y_tree::Node { role: a11y_tree::Role::Window, ..Default::default() }, Area::Empty);
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
            drop(frame);
            drop(draw);
            let lists = &cx.draw_lists;
            cx.a11y.finish_frame(Area::Empty, |id| lists.checked_index(id).map(|l| l.redraw_id), |_| None, |_| None,
                |_, bytes| { accessible.apply(a11y_tree::wire::decode_update(&bytes).unwrap()); });
            accessible.nodes.values().filter(|node|
                node.role == a11y_tree::Role::Button && node.actions.contains(a11y_tree::Actions::PRESS)).cloned().collect::<Vec<_>>()
        };
        let buttons = draw_buttons(&mut app, cx);
        // An injected press is the test driver's: it opens only in a verified test home.
        let test_home = std::cell::Cell::new(false);
        let press_event = |widget: &WidgetRef| Event::A11yAction(a11y_tree::ActionRequest {
            req: 0, node: a11y_tree::NodeId::widget(widget.widget_uid().0),
            action: a11y_tree::Actions::PRESS, arg: a11y_tree::ActionArg::None,
        });
        let dispatch = |app: &mut App, cx: &mut Cx, widget: WidgetRef, injected: bool| {
            let event = press_event(&widget);
            let consent = makepad_create::consent::decide(injected.then(|| "the shell test".to_string()), test_home.get());
            let opening = app.call_opening_buttons(cx, TranscriptInput::opening(&event, consent));
            let mut actions = cx.capture_actions(|cx| app.ui.handle_event(cx, &event, &mut Scope::with_data(&mut app.view)));
            app.consented_call_presses(cx, &event, &mut actions, &opening);
            let watching_on_release = app.view.call.share.watching.is_some();
            app.call_actions(cx, &actions);
            app.share_window_actions(cx, &actions);
            app.share_panel_actions(cx, &actions);
            app.shell_actions(cx, &actions);
            watching_on_release
        };
        let press = |app: &mut App, cx: &mut Cx, widget: WidgetRef, title: &str, injected: bool| {
            let id = a11y_tree::NodeId::widget(widget.widget_uid().0);
            assert!(buttons.iter().any(|b| b.id == id && b.name == title), "missing pressable button named {title}");
            dispatch(app, cx, widget, injected)
        };
        let projects = app.view.tree.snapshot().projects.clone();
        let rail = app.ui.portal_list(cx, ids!(tree.list));
        let tiles = app.ui.portal_list(cx, ids!(home_grid.list)).get_item(1).expect("a visible row of project tiles").1;
        let town = app.view.tree.snapshot().label.clone();
        let town_id = app.model.as_ref().unwrap().open_id();
        let town_button = rail.get_item(0).unwrap().1.widget(cx, ids!(open));
        let share_button = rail.get_item(1).unwrap().1.widget(cx, ids!(open));
        let issue_row = app.view.tree.lines.iter().position(|r| r.node == Node::Issues(projects[0].id)).unwrap();
        let issues = rail.get_item(issue_row).unwrap().1.widget(cx, ids!(open));
        press(&mut app, cx, issues, "Issues", false);
        assert_eq!(app.current_node(), Some(Node::Issues(projects[0].id)), "Issues opens in the selected project");
        assert!(draw_buttons(&mut app, cx).iter().any(|b| b.name == "Share visual issue"), "the issue list offers capture");
        let issue_page = app.ui.widget(cx, ids!(issues_page));
        let choose = press_event(&issue_page.widget(cx, ids!(share_issue)));
        app.capture_ui_actions(cx, &choose, true, Some(TranscriptInput::Choose));
        app.issue_link_events(cx, &choose, &[]);
        draw_buttons(&mut app, cx);
        assert!(issue_page.widget(cx, ids!(choices)).visible(), "Share visual issue opens the app picker");
        assert_eq!(issue_page.widget(cx, ids!(no_windows)).text(), "Town's app link is not running, so it cannot list apps.",
            "the issue picker shows the shared picker's empty-list explanation");
        app.open_page(cx, Page::Home);
        draw_buttons(&mut app, cx);
        let sharing = "Ada is sharing Drawing";
        assert!(!press(&mut app, cx, share_button.clone(), sharing, true), "an injected release cannot subscribe");
        assert_eq!(app.current_node(), Some(Node::Live(Live::Share(session))), "queued navigation still opens the offer");
        let page = app.ui.widget(cx, ids!(live_share));
        assert!(page.visible() && page.widget(cx, ids!(watch)).visible() && app.view.call.share.watching.is_none(),
            "the viewer can choose Watch on the open page");
        assert_eq!(page.label(cx, ids!(status)).text(), sharing);
        assert!(!dispatch(&mut app, cx, page.widget(cx, ids!(watch)), true), "injected Watch on the page is refused");
        test_home.set(true);
        assert!(dispatch(&mut app, cx, page.widget(cx, ids!(watch)), true),
            "in a verified test home the test driver's Watch subscribes, as a walk needs");
        let close = makepad_create::call::CallAction::Press(makepad_create::call::Press::CloseWatch);
        if app.call.call_actions(cx, close) { app.show_call(cx); }
        assert!(app.call.shown().share.watching.is_none());
        test_home.set(false);
        press(&mut app, cx, town_button.clone(), &town, false);
        assert!(press(&mut app, cx, share_button.clone(), sharing, false), "a local row press subscribes on the original release");
        assert_eq!(app.current_node(), Some(Node::Live(Live::Share(session))));
        assert!(page.widget(cx, ids!(share_picture_panel)).visible());
        app.open_page(cx, Page::Home);
        assert!(!page.visible() && app.call.shown().share.watching.is_none(), "leaving for another page closes the watch");
        let other = [7; 16];
        port.messages_in.try_send(Message::Share { session: other, title: "Notes".into(), w: 600., h: 400., ext: Default::default() }.encode()).unwrap();
        app.call.poll(cx);
        app.show_call(cx);
        let buttons_now = draw_buttons(&mut app, cx);
        let notes = rail.get_item(2).unwrap().1.widget(cx, ids!(open));
        for (row, title) in [(&share_button, sharing), (&notes, "Ada is sharing Notes")] {
            assert!(buttons_now.iter().any(|b| b.id == a11y_tree::NodeId::widget(row.widget_uid().0) && b.name == title));
        }
        let project_share = app.ui.widget(cx, ids!(project_page.call_share));
        // Open the town's project so the project header binds the current call snapshot.
        app.open_node(cx, Node::Project(projects[0].id));
        draw_buttons(&mut app, cx);
        assert!(project_share.visible() && project_share.borrow::<Button>().unwrap().enabled(),
            "Share stays enabled on the linked project while offers are open");
        dispatch(&mut app, cx, share_button.clone(), false);
        assert_eq!((app.view.share_page(), app.view.watched_share()), (Some(session), Some(session)));
        dispatch(&mut app, cx, notes.clone(), false);
        assert_eq!((app.view.share_page(), app.view.watched_share()), (Some(other), Some(other)));
        assert_eq!(page.label(cx, ids!(status)).text(), "Ada is sharing Notes");
        dispatch(&mut app, cx, page.widget(cx, ids!(close_watch)), false);
        assert_eq!(app.view.watched_share(), None);
        dispatch(&mut app, cx, share_button.clone(), false);
        assert_eq!((app.view.share_page(), app.view.watched_share()), (Some(session), Some(session)));
        port.messages_in.try_send(Message::ShareOff { session: other, reason: makepad_create::call::ShareEndReason::Stopped }.encode()).unwrap();
        app.call.poll(cx);
        app.show_call(cx);
        assert_eq!(app.view.watched_share(), Some(session), "ending the other page leaves this picture open");
        app.open_page(cx, Page::Home);
        draw_buttons(&mut app, cx);
        let window = app.ui.window(cx, ids!(share_popout));
        let window_id = window.window_id().unwrap();
        let closed = Event::WindowClosed(WindowClosedEvent { window_id });
        let pop = page.widget(cx, ids!(share_pop_out));
        let scale = page.widget(cx, ids!(share_scale));
        assert!(!scale.visible(), "the unwatched offer hides the scale button");
        assert!(!pop.visible() && !cx.windows[window_id].is_created);
        assert!(press(&mut app, cx, share_button.clone(), sharing, false));
        let center = page.widget(cx, ids!(share_picture_panel));
        let popped = window.widget(cx, ids!(popout_picture));
        assert!(scale.visible());
        assert_eq!(scale.text(), "1:1");
        let mut measure = |app: &mut App, cx: &mut Cx| {
            let actions = cx.capture_actions(|cx| { draw_buttons(app, cx); });
            actions.iter().filter_map(|a| a.as_widget_action()).filter(|w| w.widget_uid == center.widget_uid())
                .find_map(|w| w.action.downcast_ref::<makepad_create::call::SharePanelSize>().copied())
        };
        let fitted = measure(&mut app, cx).expect("the page starts at Fit");
        dispatch(&mut app, cx, scale.clone(), true);
        assert_eq!(scale.text(), "Fit");
        let actual = measure(&mut app, cx).expect("1:1 reports the full window");
        assert_eq!((actual.session, actual.width, actual.height), (session, 1600, 1000));
        assert!(actual.width > fitted.width && actual.height > fitted.height);
        let viewport = center.area().rect(cx);
        let point = viewport.pos.round() + dvec2(8., 8.);
        let wheel = |app: &mut App, cx: &mut Cx, abs, dy, shift| {
            use makepad_create::{app_link::AppInput, call::control::PanelInput};
            let event = Event::Scroll(event::ScrollEvent {
                window_id: WindowId(0, 1), abs, scroll: dvec2(0., dy), modifiers: KeyModifiers { shift, ..Default::default() },
                handled_x: Default::default(), handled_y: Default::default(), is_mouse: true, time: 0., phase: event::ScrollPhase::None,
            });
            let actions = cx.capture_actions(|cx| center.handle_event(cx, &event, &mut Scope::with_data(&mut app.view)));
            actions.iter().filter_map(|a| a.as_widget_action()).find_map(|w| match &w.action.downcast_ref::<PanelInput>()?.event {
                AppInput::Scroll { x, y, dy, .. } => Some((*x, *y, *dy)), _ => None,
            })
        };
        assert_eq!(wheel(&mut app, cx, point, 300., true), None);
        assert_eq!(wheel(&mut app, cx, point, 200., false), None);
        app.view.call.share.control_session = Some(session);
        app.view.call.share.control.active = Some([1; 16]);
        assert!(measure(&mut app, cx).is_none(), "panning and takeover preserve the subscription");
        assert_eq!(wheel(&mut app, cx, point, 12., false), Some((308., 208., 12.)), "the controlled app receives the panned point");
        let right_bar = dvec2(viewport.pos.x + viewport.size.x - 2., viewport.center().y);
        assert_eq!(wheel(&mut app, cx, right_bar, 20., false), None, "the bar still scrolls locally during control");
        assert_eq!(wheel(&mut app, cx, point, 12., false), Some((308., 228., 12.)));
        app.view.call.share.control.active = None;
        dispatch(&mut app, cx, scale.clone(), true);
        assert_eq!(scale.text(), "1:1");
        let restored = measure(&mut app, cx).expect("Fit reports the panel size again");
        assert_eq!((restored.width, restored.height), (fitted.width, fitted.height));
        // Either view of the watched picture reaches the takeover grant check; a closed window's does not.
        let check_inputs = |app: &mut App, cx: &mut Cx, window_open: bool| {
            use makepad_create::{app_link::AppInput, call::control::PanelInput};
            for (panel, reaches) in [(&center, true), (&popped, window_open)] {
                let before = app.call.inputs_refused();
                let actions = cx.capture_actions(|cx| cx.widget_action(panel.widget_uid(),
                    PanelInput { session, grant: [0; 16], event: AppInput::Text("a".into()) }));
                app.share_panel_actions(cx, &actions);
                assert_eq!(app.call.inputs_refused(), before + u64::from(reaches), "the open views carry control input");
            }
        };
        let window_reports = |app: &mut App, cx: &mut Cx, width, height| {
            let actions = cx.capture_actions(|cx| cx.widget_action(popped.widget_uid(),
                makepad_create::call::SharePanelSize { session, width, height }));
            app.share_panel_actions(cx, &actions);
        };
        // Every new frame reaches the page's picture, with or without the window.
        let frames = std::cell::Cell::new(0u8);
        let mut docked_plays = |app: &mut App, cx: &mut Cx, when: &str| {
            frames.set(frames.get() + 1);
            let picture = std::sync::Arc::new(makepad_call::driver::codec::Picture {
                width: 16, height: 10, nv12: vec![frames.get(); 240], pts_100ns: frames.get().into() });
            app.view.call.share.picture = Some(picture.clone());
            draw_buttons(app, cx);
            let drawn = center.borrow::<makepad_create::call::SharePanel>().and_then(|p| p.drawn());
            assert!(drawn.is_some_and(|d| std::sync::Arc::ptr_eq(&d, &picture)), "the page's picture plays {when}");
        };
        app.call.share_panel_size(session, makepad_create::call::ShareSurface::Page, restored.width, restored.height);
        let page_view = app.call.share_subscription();
        assert_eq!((page_view.width, page_view.height), (restored.width, restored.height));
        docked_plays(&mut app, cx, "before the window opens");
        let native = makepad_call::driver::View { width: 1600, height: 1000, fps: 30 };
        let draw_popout = |app: &mut App, cx: &mut Cx, size: DVec2| cx.capture_actions(|cx| {
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            let pass = DrawPass::new(&mut frame);
            pass.set_size(&mut frame, size);
            frame.begin_pass(&pass, Some(1.));
            let mut list = DrawList2d::new(&mut frame);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            window.widget(&frame, ids!(share_view)).draw_all(&mut frame, &mut Scope::with_data(&mut app.view));
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
        });
        let click_caption = |app: &mut App, cx: &mut Cx, key, expected: CxOsOp| {
            let row = popped.borrow::<makepad_create::call::SharePanel>().unwrap().window_buttons().unwrap().clone();
            let rect = row.child(key).area().clipped_rect(cx);
            assert!(rect.size.x > 0. && rect.size.y > 0., "{key} is drawn");
            let abs = rect.center();
            let button = MouseButton::PRIMARY;
            cx.fingers.first_mouse_button = Some((button, window_id));
            let down = Event::MouseDown(MouseDownEvent { abs, button, window_id, modifiers: Default::default(),
                handled: Default::default(), time: 1. });
            let _ = cx.capture_actions(|cx| popped.handle_event(cx, &down, &mut Scope::with_data(&mut app.view)));
            cx.fingers.first_mouse_button = None;
            let up = Event::MouseUp(MouseUpEvent { abs, button, window_id, modifiers: Default::default(), time: 1.1 });
            let actions = cx.capture_actions(|cx| popped.handle_event(cx, &up, &mut Scope::with_data(&mut app.view)));
            cx.fingers = Default::default();
            assert!(app.popout_window_op(cx, &window, &actions) == Some(expected), "{key} requests the operation on the pop-out");
            app.share_window_actions(cx, &actions);
        };
        // Open and close the window again and again: the page keeps playing, and the stream keeps its size.
        for round in 0..3 {
            assert!(pop.visible(), "Pop out is offered again in round {round}");
            dispatch(&mut app, cx, pop.clone(), true);
            assert!(app.view.share_in_window(), "an injected Pop out opens the window on an already watched picture");
            assert_eq!(cx.windows[window_id].create_title, makepad_platform::badge_window_title("Drawing".into()));
            assert_eq!(page.label(cx, ids!(status)).text(), sharing, "the page has no placeholder while the window is open");
            assert!(page.widget(cx, ids!(embedded)).visible() && center.visible() && popped.visible() && scale.visible(),
                "the page keeps its picture and controls beside the window");
            docked_plays(&mut app, cx, "while the window is open");
            check_inputs(&mut app, cx, true);
            let expected = if round == 0 { page_view } else { native };
            window_reports(&mut app, cx, if round == 0 { page_view.width / 2 } else { 1600 }, if round == 0 { page_view.height / 2 } else { 1000 });
            assert_eq!(app.call.share_subscription(), expected, "the stream is the larger view's size");
            assert!(window.widget(cx, ids!(popout_fullscreen)).is_empty(), "fullscreen belongs to the window system");
            cx.windows[window_id].window_geom.is_fullscreen = true;
            let chrome = if round == 0 { ResolvedChrome::Lights } else { ResolvedChrome::Buttons };
            let set_controls = |cx: &mut Cx, expanded| popped.borrow_mut::<makepad_create::call::SharePanel>()
                .unwrap().set_window_controls(cx, chrome, expanded);
            set_controls(cx, true);
            let actions = draw_popout(&mut app, cx, size);
            let full = popped.borrow::<makepad_create::call::SharePanel>().unwrap().view_rect;
            assert_eq!(full, Rect { pos: (size - dvec2(16., 10.)) * 0.5, size: dvec2(16., 10.) },
                "system fullscreen centers the decoded frame at one device pixel per pixel");
            let requested = actions.iter().filter_map(|a| a.as_widget_action())
                .find_map(|w| w.action.downcast_ref::<makepad_create::call::SharePanelSize>()).unwrap();
            assert_eq!((requested.width, requested.height), (16384, 10240),
                "the pop-out asks for every pixel at the shared aspect; the sharer's caps bound it");
            if round == 1 { click_caption(&mut app, cx, id!(restore), CxOsOp::RestoreWindow(window_id)); }
            cx.windows[window_id].window_geom.is_fullscreen = false;
            set_controls(cx, false);
            if round == 0 {
                // With a larger picture both scroll bars are present. Query through the app so
                // the Window's own classification is part of every assertion.
                let frame = app.view.call.share.picture.take();
                draw_popout(&mut app, cx, size);
                let drag = |app: &mut App, cx: &mut Cx, abs| {
                    let response = std::rc::Rc::new(std::cell::Cell::new(WindowDragQueryResponse::NoAnswer));
                    app.handle_event(cx, &Event::WindowDragQuery(WindowDragQueryEvent { window_id, abs, response: response.clone() }));
                    response.get()
                };
                use WindowDragQueryResponse::{Caption, Client};
                let lights = popped.borrow::<makepad_create::call::SharePanel>().unwrap().window_buttons().unwrap().clone();
                for (i, key) in [id!(close), id!(min), id!(max)].into_iter().enumerate() {
                    let rect = lights.child(key).area().clipped_rect(cx);
                    assert_eq!(rect, Rect { pos: dvec2(8. + i as f64 * 20., 4.), size: dvec2(20., 20.) }, "{key} uses the hosted light geometry");
                    assert!(matches!(drag(&mut app, cx, rect.center()), Client), "{key} keeps its press");
                }
                click_caption(&mut app, cx, id!(min), CxOsOp::MinimizeWindow(window_id));
                click_caption(&mut app, cx, id!(max), CxOsOp::MaximizeWindow(window_id));
                let ask = popped.widget(cx, ids!(request_control)).area().clipped_rect(cx);
                let close = popped.widget(cx, ids!(close_popout)).area().clipped_rect(cx);
                let chip = popped.widget(cx, ids!(chip)).area().clipped_rect(cx);
                let name = popped.widget(cx, ids!(stream_name)).area().clipped_rect(cx).center();
                let panel = popped.area().clipped_rect(cx);
                assert!((0.5 * (chip.pos.x + ask.pos.x + ask.size.x) - panel.center().x).abs() <= 1.,
                    "the chip and button sit together at the visible picture's horizontal centre");
                assert!((chip.pos.y - panel.pos.y).abs() <= 1., "the chip sits at the top of the visible picture");
                assert!(matches!(drag(&mut app, cx, panel.center()), Caption), "the picture body moves the ungranted window");
                assert!(matches!(drag(&mut app, cx, ask.center()), Client), "the control button keeps its press");
                assert!(matches!(drag(&mut app, cx, close.center()), Client), "the close button keeps its press");
                for (point, axis) in [(dvec2(panel.center().x, panel.pos.y + panel.size.y - 2.), "horizontal"),
                    (dvec2(panel.pos.x + panel.size.x - 2., panel.center().y), "vertical")] {
                    assert!(matches!(drag(&mut app, cx, point), Client), "the {axis} scroll bar keeps its press");
                }
                app.view.call.share.control_session = Some(session);
                app.view.call.share.control.active = Some([1; 16]);
                assert!(matches!(drag(&mut app, cx, panel.center()), Client), "the granted picture takes input");
                assert!(matches!(drag(&mut app, cx, name), Caption), "the name pill moves even the granted window");
                assert!(matches!(drag(&mut app, cx, close.center()), Client), "the close inside the pill still clicks during control");
                cx.windows[window_id].window_geom.is_fullscreen = true;
                assert!(matches!(drag(&mut app, cx, name), Client), "the name pill stays put in fullscreen");
                app.view.call.share.control.active = None;
                assert!(matches!(drag(&mut app, cx, panel.center()), Client), "the ungranted fullscreen picture stays put");
                cx.windows[window_id].window_geom.is_fullscreen = false;
                app.view.call.share.picture = frame;
                let dock_request = center.widget(cx, ids!(request_control));
                let pop_request = popped.widget(cx, ids!(request_control));
                assert_eq!(dock_request.text(), "Ask for control");
                dispatch(&mut app, cx, dock_request.clone(), false);
                assert_eq!(app.view.call.share.control.asking, Some(session));
                assert_eq!(pop_request.text(), "Asking…", "both views show the same outstanding ask");
                dispatch(&mut app, cx, pop_request.clone(), false);
                assert_eq!(app.view.call.share.control.asking, None);
                assert_eq!(dock_request.text(), "Ask for control", "cancel in either view clears the ask");
                app.view.call.share.control.allowed = Some(session);
                app.view.sync_live_share(cx, &app.ui);
                for request in [&dock_request, &pop_request] {
                    assert_eq!(request.text(), "Take control", "either view can use the share's right");
                }
                app.view.call.share.control_session = Some(session);
                app.view.call.share.control.active = Some([3; 16]);
                app.view.sync_live_share(cx, &app.ui);
                for panel in [&center, &popped] {
                    assert_eq!(panel.widget(cx, ids!(request_control)).text(), "Hand back");
                    assert!(panel.widget(cx, ids!(controlling)).visible(), "the chip marks the active spell");
                }
                app.show_call(cx);
                app.fit_window(cx, 400., false);
                assert_eq!((app.view.page, app.view.pane), (Page::Live(Live::Share(session)), Pane::Chat));
                assert!(app.call.shown().share.watching.is_some(), "the window's watch outlives a pane change");
            } else if round == 1 {
                app.open_page(cx, Page::Home);
                assert!(!page.visible() && app.call.shown().share.watching.is_some(), "the window's watch outlives navigation");
                cx.windows[window_id].window_geom.is_maximized = true;
                set_controls(cx, true);
                draw_popout(&mut app, cx, size);
                let row = popped.borrow::<makepad_create::call::SharePanel>().unwrap().window_buttons().unwrap().clone();
                for (i, key) in [id!(min), id!(restore), id!(close)].into_iter().enumerate() {
                    let rect = row.child(key).area().clipped_rect(cx);
                    assert_eq!(rect, Rect { pos: dvec2(size.x - 138. + i as f64 * 46., 0.), size: dvec2(46., 29.) },
                        "{key} uses the top-right button geometry");
                    assert!(!popped.borrow::<makepad_create::call::SharePanel>().unwrap().caption_at(cx, rect.center(), false),
                        "{key} keeps its press while maximised");
                }
                click_caption(&mut app, cx, id!(restore), CxOsOp::RestoreWindow(window_id));
                let frame = app.view.call.share.picture.take();
                draw_popout(&mut app, cx, dvec2(230., 160.));
                assert!(popped.widget(cx, ids!(chip)).area().clipped_rect(cx).pos.y >= 29.,
                    "in a narrow pop-out the centred chip clears the window buttons");
                app.view.call.share.picture = frame;
                cx.windows[window_id].window_geom.is_maximized = false;
                click_caption(&mut app, cx, id!(close), CxOsOp::CloseWindow(window_id));
                assert_eq!(app.view.share_popout, makepad_create::lists::SharePopout::Closing(session),
                    "the caption close returns to the watched page");
            }
            if round == 2 {
                dispatch(&mut app, cx, popped.widget(cx, ids!(close_popout)), false);
                assert_eq!(app.view.share_popout, makepad_create::lists::SharePopout::Closing(session),
                    "the chip closes just its watched window");
            }
            let accept_close = std::rc::Rc::new(std::cell::Cell::new(true));
            let request = Event::WindowCloseRequested(WindowCloseRequestedEvent { window_id, accept_close: accept_close.clone() });
            app.handle_event(cx, &request);
            assert!(accept_close.get() && !app.closing_store, "closing the share window leaves the main window running");
            app.share_window_event(cx, &closed);
            assert_eq!(app.current_node(), Some(Node::Live(Live::Share(session))), "closing the window returns to the share's page");
            assert!(app.call.shown().share.watching.is_some() && center.visible() && !popped.visible());
            assert_eq!(app.call.share_subscription(), expected, "closing the window keeps the stream's size");
            docked_plays(&mut app, cx, "after the window closed");
            check_inputs(&mut app, cx, false);
            if round == 0 {
                assert_eq!(app.view.pane, Pane::Page, "closing the window shows the picture even when Chat covered its page");
                app.fit_window(cx, 1050., false);
            }
        }
        app.open_node(cx, Node::Town(town_id));
        for (project, path) in projects.iter().zip([ids!(tile_a.open), ids!(tile_b.open)]) {
            let tile = tiles.widget(cx, path);
            let index = app.view.tree.lines.iter().position(|line| line.node == Node::Project(project.id)).unwrap();
            let row = rail.get_item(index).unwrap().1.widget(cx, ids!(open));
            for button in [tile, row] {
                press(&mut app, cx, button, &project.title, false);
                assert_eq!(app.current_node(), Some(Node::Project(project.id)), "press opens the named project");
                press(&mut app, cx, town_button.clone(), &town, false);
                assert_eq!(app.current_node(), Some(Node::Town(town_id)), "press opens the named town");
            }
        }
        assert!(press(&mut app, cx, share_button, sharing, false));
        dispatch(&mut app, cx, pop, true);
        port.messages_in.try_send(Message::ShareOff { session, reason: makepad_create::call::ShareEndReason::Stopped }.encode()).unwrap();
        app.call.poll(cx);
        app.show_call(cx);
        assert_eq!(app.view.share_popout, makepad_create::lists::SharePopout::Closing(session), "ending the share closes its window");
        app.share_window_event(cx, &closed);
        assert_eq!(app.view.page, Page::Home);
        assert!(app.view.call.share.watching.is_none() && !window.widget(cx, ids!(popout_picture)).visible());
        let chat_edge = app.ui.widget(cx, ids!(chat_edge));
        let close_ended = page.widget(cx, ids!(close_ended));
        for (session, detached) in [([8; 16], false), ([9; 16], true)] {
            let share = Live::Share(session);
            port.messages_in.try_send(Message::Share { session, title: "Drawing".into(), w: 96., h: 64., ext: Default::default() }.encode()).unwrap();
            app.call.poll(cx);
            app.show_call(cx);
            app.open_node(cx, Node::Live(share));
            draw_buttons(&mut app, cx);
            dispatch(&mut app, cx, page.widget(cx, ids!(watch)), false);
            assert!(center.visible() && chat_edge.visible() && !close_ended.visible(), "the embedded picture folds chat while watching");
            if detached {
                dispatch(&mut app, cx, page.widget(cx, ids!(share_pop_out)), true);
                app.open_node(cx, Node::Project(projects[0].id));
                assert!(app.view.share_in_window(), "the detached picture stays open over another page");
            }
            port.messages_in.try_send(Message::ShareOff { session, reason: makepad_create::call::ShareEndReason::AppClosedOrRestarted }.encode()).unwrap();
            app.call.poll(cx);
            app.show_call(cx);
            if detached {
                assert_eq!(app.view.share_popout, makepad_create::lists::SharePopout::Closing(session), "the ended picture closes its window");
                app.share_window_event(cx, &closed);
            }
            assert_eq!(app.view.page, Page::Live(share), "the notice belongs on the ended share's page");
            let row = app.view.tree.lines.iter().find(|line| line.node == Node::Live(share)).expect("the ended share keeps its row");
            assert!(row.open, "the ended row stays selected");
            assert_eq!(row.label, "Drawing closed or restarted");
            assert_eq!(page.label(cx, ids!(status)).text(), "Drawing closed or restarted.");
            assert!(page.visible() && !center.visible() && !popped.visible() && !chat_edge.visible(), "ending restores chat and removes both pictures");
            assert!(!page.widget(cx, ids!(embedded)).visible(), "the live controls leave the page");
            let buttons = draw_buttons(&mut app, cx);
            assert!(buttons.iter().any(|b| b.id == a11y_tree::NodeId::widget(close_ended.widget_uid().0) && b.name == "Close"),
                "the ended page offers a pressable Close");
            if detached {
                app.open_node(cx, Node::Project(projects[0].id));
                assert_eq!(app.view.page, Page::Project, "leaving the notice opens the chosen page");
            } else {
                dispatch(&mut app, cx, close_ended.clone(), true);
                assert_eq!(app.view.page, Page::Home, "Close on the notice returns Home");
            }
            app.show_call(cx);
            assert!(!app.view.has_live(share) && app.view.tree.lines.iter().all(|line| line.node != Node::Live(share)),
                "a later call update must not bring back a dismissed row");
        }
        app.open_page(cx, Page::Home);
        app.fit_window(cx, 400., false);
        assert_eq!(app.view.pane, Pane::Chat);
        app.call.link_down(cx, [2; 65]);
        let port = app.call.link_up(cx, [1; 65], [2; 65], "").unwrap();
        let call_id = [6; 16];
        port.messages_in.try_send(Message::Ring { call_id, secret: [3; 32], video: false, ext: Default::default() }.encode()).unwrap();
        app.call.poll(cx);
        app.show_call(cx);
        assert_eq!((app.view.page, app.view.pane), (Page::Home, Pane::Chat), "an incoming ring leaves the open pane alone");
        let call_buttons = draw_buttons(&mut app, cx);
        let row = rail.get_item(1).unwrap().1.widget(cx, ids!(open));
        assert!(call_buttons.iter().any(|b| b.id == a11y_tree::NodeId::widget(row.widget_uid().0) && b.name == "Peer is calling"));
        let entry = app.ui.widget(cx, ids!(compact_call));
        assert!(call_buttons.iter().any(|b| b.id == a11y_tree::NodeId::widget(entry.widget_uid().0) && b.name == "Peer is calling"));
        dispatch(&mut app, cx, entry, false);
        assert_eq!((app.view.page, app.view.pane), (Page::Live(Live::Call), Pane::Page));
        assert!(app.call.view().ringing(), "opening a ringing row leaves the call unanswered");
        draw_buttons(&mut app, cx);
        let answer = app.ui.widget(cx, ids!(live_call.call_answer));
        assert!(answer.visible());
        dispatch(&mut app, cx, answer.clone(), true);
        assert!(app.call.view().ringing(), "an injected Answer is refused on the call page");
        dispatch(&mut app, cx, answer, false);
        assert_eq!(app.call.view().phase, makepad_create::call::Phase::Live);
        // Opening the already selected row must preserve the original return page.
        dispatch(&mut app, cx, row, false);
        draw_buttons(&mut app, cx);
        let hang = app.ui.widget(cx, ids!(live_call.hang_up));
        dispatch(&mut app, cx, hang, true);
        assert_eq!(app.call.view().phase, makepad_create::call::Phase::Idle);
        assert_eq!((app.view.page, app.view.pane), (Page::Home, Pane::Chat), "hang-up restores the previous page and pane");
        assert!(!app.ui.widget(cx, ids!(live_call)).visible());
        assert!(app.view.tree.lines.iter().all(|line| line.node != Node::Live(Live::Call)));
        app.fit_window(cx, 1050., false);
        let project = app.ui.widget(cx, ids!(project_page));
        let project_menu = project.widget(cx, ids!(call_share));
        let project_choices = project.widget(cx, ids!(choices));
        let call = app.ui.widget(cx, ids!(live_call));
        let call_menu = call.widget(cx, ids!(call_share));
        let call_choices = call.widget(cx, ids!(choices));
        let opening = makepad_create::call::opening_uids(cx, &app.ui);
        for control in [&project_menu, &call_menu, &project_choices, &call_choices] {
            assert!(opening.contains(&control.widget_uid()), "both Share buttons and both picker emitters need the local-release gate");
        }

        app.open_node(cx, Node::Project(projects[0].id));
        let buttons = draw_buttons(&mut app, cx);
        assert!(project_menu.visible() && !project_menu.disabled(cx));
        assert!(buttons.iter().any(|b| b.id == a11y_tree::NodeId::widget(project_menu.widget_uid().0) && b.name == "Share"),
            "a linked, idle project exposes a pressable Share beside Call");
        dispatch(&mut app, cx, project_menu.clone(), true);
        assert!(app.call.shown().share.choices.is_none(), "an injected project Share outside a test home opened the picker");
        dispatch(&mut app, cx, project_menu.clone(), false);
        let mut empty_apps = makepad_create::app_link::AppLinks::default();
        app.call.share_app_events(&mut empty_apps, &[]);
        app.show_call(cx);
        draw_buttons(&mut app, cx);
        assert!(project_choices.visible(), "the local project Share press must show its picker under the header");
        let empty = project.widget(cx, ids!(choices.list.no_windows));
        assert_eq!(empty.text(), "Town's app link is not running, so it cannot list apps.", "the project picker lost its empty-list explanation");
        assert_eq!(app.call.view().phase, makepad_create::call::Phase::Idle, "opening Share on the project started voice");

        // The row is still on screen, but its window has gone from the controller's snapshot.
        let stale = makepad_create::call::ShareRow { widget_id: id!(departed_window),
            choice: makepad_create::call::ShareChoice::Row(u64::MAX), label: "Departed window".into(), selectable: true, thumbnail: None };
        app.view.call.share.choices = Some(vec![stale].into());
        draw_buttons(&mut app, cx);
        let stale_row = project_choices.widget(cx, ids!(list.departed_window));
        assert!(!stale_row.is_empty(), "the stale-row check needs a button actually drawn by the project picker");
        dispatch(&mut app, cx, stale_row.clone(), true);
        assert!(app.call.shown().share.problem.is_none() && app.call.shown().share.offers.is_empty(),
            "an injected project row outside a test home must never reach window selection");
        dispatch(&mut app, cx, stale_row, false);
        assert_eq!(app.call.shown().share.problem.as_deref(), Some("The selected window is no longer in the list."),
            "the local project row must reach the controller's stale-window check");
        assert!(app.call.shown().share.offers.is_empty(), "a departed window cannot become a share");
        draw_buttons(&mut app, cx);
        assert_eq!(project.label(cx, ids!(share_problem)).text(), "The selected window is no longer in the list.",
            "the project must explain why the selected share could not start");

        port.messages_in.try_send(Message::Ring { call_id: [7; 16], secret: [4; 32], video: false, ext: Default::default() }.encode()).unwrap();
        app.call.poll(cx);
        app.show_call(cx);
        app.open_node(cx, Node::Live(Live::Call));
        draw_buttons(&mut app, cx);
        dispatch(&mut app, cx, call.widget(cx, ids!(call_answer)), false);
        draw_buttons(&mut app, cx);
        assert!(call.visible() && call_choices.visible() && !project.visible(), "the call page must present the already open picker");
        dispatch(&mut app, cx, call_menu.clone(), false);
        assert!(app.call.shown().share.choices.is_none(), "the call control must close the same picker the project opened");
        dispatch(&mut app, cx, call_menu, false);
        let snapshot = app.call.shown().share.choices.expect("the live call must reopen the picker");
        app.open_node(cx, Node::Project(projects[0].id));
        draw_buttons(&mut app, cx);
        assert!(project.visible() && project_menu.visible() && project_choices.visible() && !call.visible(),
            "the project shows the one picker while a voice call is live");
        assert!(std::sync::Arc::ptr_eq(&snapshot, &app.call.shown().share.choices.unwrap()),
            "changing pages replaced the controller's picker snapshot");
        app.call.link_down(cx, [2; 65]);
        app.show_call(cx);
        // Change just the town's membership while the call snapshot stays idle and unlinked.
        use makepad_create::view::{MemberId, MemberSeat, Seat};
        for (peer, reason) in [(None, "No one else is connected yet"),
            (Some((2, "Anna")), "Anna isn't connected yet"), (Some((3, "Bea")), "No one else is connected yet")] {
            if let Some((key, name)) = peer {
                app.view.members.seats.push(MemberSeat { id: MemberId::Computer([key; 65]), seat: Seat::Peer,
                    name: name.into(), presence: makepad_create::view::Presence::Away, status: String::new() });
                app.view.member_revision += 1;
            }
            let buttons = draw_buttons(&mut app, cx);
            assert!(project_menu.visible() && project_menu.disabled(cx) && !project_choices.visible(),
                "unlink must leave Share disabled and close its picker");
            assert_eq!(project_menu.text(), "Share");
            let menu = buttons.iter().find(|b| b.id == a11y_tree::NodeId::widget(project_menu.widget_uid().0)).unwrap();
            assert_eq!(menu.name, format!("Share: {reason}"));
            assert_eq!(project.widget(cx, ids!(project_call.call_start)).text(), reason);
        }
        let actions = cx.capture_actions(|cx| app.ui.handle_event(cx, &press_event(&project_menu), &mut Scope::with_data(&mut app.view)));
        assert!(!actions.iter().filter_map(|a| a.as_widget_action()).any(|w| w.action.downcast_ref::<makepad_create::call::CallAction>().is_some()),
            "disabled Share emitted a press");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rename_in_place_and_delete_from_the_menu() {
        let (mut cx, mut app, dir) = app("rename");
        let cx = &mut cx;
        let project = app.view.tree.snapshot().projects[0].id;
        app.open_node(cx, Node::Project(project));
        app.add_to_town(cx, NewKind::Doc);
        let doc = app.view.tree.snapshot().project(project).unwrap().rows.iter().find(|r| r.kind == RowKind::Doc).unwrap().id;
        app.open_node(cx, Node::Project(project));
        app.add_to_town(cx, NewKind::Board);
        let board = app.view.open_board.unwrap();
        assert_eq!(app.title_shown.as_ref().map(|t| t.0), Some(board), "the title field shows the open board's id");

        assert_eq!(app.edit_title(cx, "  Print layout ", false), None, "a rename is written");
        assert_eq!(titles(&app, board), ["Print layout"; 3], "the row, the tree line and the tile agree");
        assert_eq!(app.title_shown.as_ref().map(|t| (t.0, t.1.as_str())), Some((board, "Print layout")), "the id stays");
        assert_eq!(app.edit_title(cx, "Other", true).as_deref(), Some("Print layout"), "Escape puts the old title back");
        assert_eq!(app.edit_title(cx, "   ", false).as_deref(), Some("Print layout"), "an empty title puts the old one back");
        assert_eq!(titles(&app, board)[0], "Print layout", "neither wrote");
        let writer = |app: &App| app.model.as_ref().unwrap().state.entities[&board].fields[&makepad_town_core::entity::field::TITLE].writer;
        let written = writer(&app);
        assert_eq!(app.edit_title(cx, "Print layout", false), None);
        assert_eq!(writer(&app), written, "an unchanged title writes nothing");

        // Delete asks once; Keep changes nothing.
        app.view.delete = Delete::Confirm(board);
        app.update_page(cx);
        assert_eq!(app.view.delete, Delete::Confirm(board), "the confirmation stays on its own page");
        app.view.delete = Delete::Closed;
        app.update_page(cx);
        assert!(app.view.tree.snapshot().row(board).is_some(), "Keep leaves the board");
        app.delete_page(cx, board);
        assert!(app.view.tree.snapshot().row(board).is_none(), "Delete removes the row");
        assert_eq!((app.view.page, app.view.workspace.current), (Page::Project, Some(project)), "and lands on the overview");
        assert!(app.view.tree.snapshot().row(doc).is_some(), "the doc stays");
        app.open_node(cx, Node::Tasks(project));
        assert_eq!(app.view.page, Page::Tasks);
        assert!(app.delete_page_refused(project), "the task list has no Delete");
        app.open_node(cx, Node::Town(app.view.tree.snapshot().town.unwrap()));
        assert!(app.delete_page_refused(app.view.tree.snapshot().town.unwrap()), "the town has no Delete here");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_towns_name_is_edited_from_home_and_the_tree_row_follows() {
        let (mut cx, mut app, dir) = app("town-name");
        let cx = &mut cx;
        let town = app.view.tree.snapshot().town.unwrap();
        let derived = app.view.tree.snapshot().label.clone();
        assert_eq!(app.title_shown.as_ref().map(|t| (t.0, t.1.clone())), Some((town, derived)), "Home's title is the town's label");
        assert_eq!(app.edit_title(cx, " Makepad ", false), None, "the rename is written");
        assert_eq!(app.title_shown.as_ref().map(|t| t.1.as_str()), Some("Makepad"), "Home follows");
        assert_eq!(app.view.tree.lines[0].label, "Makepad", "the tree's town row follows");
        assert_eq!(app.edit_title(cx, "  ", false).as_deref(), Some("Makepad"), "an empty name puts the old one back");
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
