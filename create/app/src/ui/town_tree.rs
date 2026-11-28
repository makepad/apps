//! The left rail's town tree: the towns, and under the open one each project
//! with everything it holds. Every row carries an id; titles are display text only.
//! Only the twisty folds, and fold state is this computer's own, kept in memory.

use crate::lists::{highlight, ViewData};
use crate::model::tree::{RowKind, TownEntry, TownTree};
use makepad_widgets::*;
use makepad_town_core::event::{Id, SessionId};
use crate::call::{CallView, Phase, ShareRole};
use crate::model::TownUnread;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    Share(SessionId),
    Call,
}
use std::collections::BTreeSet;

/// What a row opens. The id is the row's own: a town, a project or an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Town(Id),
    Live(Live),
    Project(Id),
    /// A project's one task list, by its project's id.
    Tasks(Id),
    Issues(Id),
    Board(Id),
    Doc(Id),
}

impl Node {
    /// The name button carries the node it showed, even if the list recycles its row.
    pub fn clicked(action: &WidgetAction) -> Option<Self> {
        if !matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { return None }
        action.data.as_ref()?.downcast_ref::<Self>().copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRow {
    pub item: Live,
    pub label: String,
}

impl LiveRow {
    pub(super) fn share(session: SessionId, role: ShareRole, peer: &str, title: &str) -> Self {
        let label = match role {
            ShareRole::Viewer { .. } => format!("{peer} is sharing {title}"),
            ShareRole::Sharer { .. } => format!("You are sharing {title}"),
        };
        Self { item: Live::Share(session), label }
    }

    pub(super) fn call(view: &CallView) -> Option<Self> {
        let peer = &view.peer_name;
        let label = match view.phase {
            Phase::Idle => return None,
            Phase::Calling => format!("Calling {peer}…"),
            Phase::Ringing => format!("{peer} is calling"),
            Phase::Live => format!("Call with {peer} · {}:{:02}", view.duration_secs / 60, view.duration_secs % 60),
        };
        Some(Self { item: Live::Call, label })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub node: Node,
    pub depth: usize,
    pub glyph: &'static str,
    pub label: String,
    pub note: String,
    /// `Some(expanded)` when the row has a twisty.
    pub fold: Option<bool>,
    pub open: bool,
    /// A town row's unread chat lines; 0 on every other row.
    pub unread: usize,
    /// Some of those lines wait on this seat: a reply, an `@`, or an approval for its AI.
    pub urgent: bool,
}

/// A town row's badge: its unread count, and whether any of it waits on this seat.
fn badge(unread: Option<&TownUnread>) -> (usize, bool) {
    unread.map_or((0, false), |u| (u.count, u.count > 0 && u.mentions_or_approvals > 0))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Name,
    Twisty,
}

#[derive(Default)]
pub struct TreeState {
    towns: Vec<TownEntry>,
    tree: TownTree,
    pub(super) live: Vec<LiveRow>,
    pub(super) live_start: Option<usize>,
    current: Option<Node>,
    folded: BTreeSet<Id>,
    /// Each town's unread count, kept here so a rebuild never drops a badge.
    unread: BTreeMap<Id, TownUnread>,
    pub lines: Vec<Line>,
}

impl TreeState {
    /// The towns this computer has joined and the open one's snapshot. Run when the model
    /// changes, never per frame: it walks the rows once.
    pub fn set(&mut self, towns: Vec<TownEntry>, tree: TownTree) {
        self.towns = towns;
        self.tree = tree;
        self.rebuild();
    }

    /// Only a change of items affects the tree layout. A new clock label edits its own line.
    pub fn set_live(&mut self, live: Vec<LiveRow>) -> bool {
        let structural = !self.live.iter().map(|r| r.item).eq(live.iter().map(|r| r.item));
        self.live = live;
        if structural {
            self.rebuild();
        } else if let Some(start) = self.live_start {
            for (line, row) in self.lines[start..].iter_mut().zip(&self.live) {
                line.label.clone_from(&row.label);
            }
        }
        structural
    }

    pub fn snapshot(&self) -> &TownTree { &self.tree }

    /// New unread counts edit the town rows in place, without a rebuild. True when a row's
    /// badge changed and the tree needs a redraw.
    pub fn set_unread(&mut self, counts: &BTreeMap<Id, TownUnread>) -> bool {
        if self.unread == *counts { return false }
        self.unread.clone_from(counts);
        let mut changed = false;
        for line in &mut self.lines {
            let Node::Town(id) = line.node else { continue };
            let shown = badge(self.unread.get(&id));
            if (line.unread, line.urgent) != shown {
                (line.unread, line.urgent) = shown;
                changed = true;
            }
        }
        changed
    }

    /// Marks the row of the open page.
    pub fn show(&mut self, current: Option<Node>) {
        if self.current == current { return }
        self.current = current;
        for line in &mut self.lines { line.open = Some(line.node) == current; }
    }

    /// A press on the row at `index`. A name returns what to open and folds nothing. A twisty
    /// folds only its own row; on a town that isn't open it returns that town to open.
    pub fn press(&mut self, index: usize, hit: Press) -> Option<Node> {
        let line = self.lines.get(index)?;
        let node = line.node;
        match (hit, line.fold) {
            (Press::Name, _) => Some(node),
            (Press::Twisty, Some(_)) => {
                let id = match node { Node::Town(id) | Node::Project(id) => id, _ => return None };
                if matches!(node, Node::Town(town) if Some(town) != self.tree.town) { return Some(node) }
                if !self.folded.remove(&id) { self.folded.insert(id); }
                self.rebuild();
                None
            }
            (Press::Twisty, None) => None,
        }
    }

    fn rebuild(&mut self) {
        let current = self.current;
        let mut lines = Vec::with_capacity(self.lines.len());
        self.live_start = None;
        let unread = &self.unread;
        let mut line = |node, depth, glyph, label: &str, note: &str, fold| {
            let (unread, urgent) = match node { Node::Town(id) => badge(unread.get(&id)), _ => (0, false) };
            lines.push(Line { node, depth, glyph, label: label.to_owned(), note: note.to_owned(), fold, open: Some(node) == current, unread, urgent });
            lines.len()
        };
        for entry in &self.towns {
            let town = entry.id;
            let expanded = Some(town) == self.tree.town && !self.folded.contains(&town);
            let start = line(Node::Town(town), 0, "", &entry.title, &entry.note, Some(expanded));
            if !expanded { continue }
            self.live_start = Some(start);
            for live in &self.live {
                line(Node::Live(live.item), 1, "◉", &live.label, "", None);
            }
            for project in &self.tree.projects {
                let expanded = !self.folded.contains(&project.id);
                line(Node::Project(project.id), 1, "▣", &project.title, "", Some(expanded));
                if !expanded { continue }
                for row in &project.rows {
                    let (node, glyph) = match row.kind {
                        RowKind::Tasks => (Node::Tasks(project.id), "✓"),
                        RowKind::Board => (Node::Board(row.id), "▦"),
                        RowKind::Doc => (Node::Doc(row.id), "≡"),
                    };
                    line(node, 2, glyph, &row.title, "", None);
                    if row.kind == RowKind::Tasks { line(Node::Issues(project.id), 2, "", "Issues", "", None); }
                }
            }
        }
        self.lines = lines;
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct TownTreeList {
    /// A town badge's fill and count, plain and when something waits on this seat.
    #[live] badge_bg: Vec4f,
    #[live] badge_urgent_bg: Vec4f,
    #[live] badge_text: Vec4f,
    #[live] badge_urgent_text: Vec4f,
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    share_buttons: Vec<WidgetUid>,
}

impl TownTreeList {
    pub(crate) fn share_buttons(&self) -> &[WidgetUid] { &self.share_buttons }
}

impl Widget for TownTreeList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            self.share_buttons.clear();
            let lines = &data.tree.lines;
            list.set_item_range(cx, 0, lines.len() + 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i == lines.len() {
                    let footer = list.item(cx, i, id!(Join));
                    let join = footer.button(cx, ids!(connect_join));
                    join.set_action_data(crate::lists::HomeAction::Join);
                    join.set_enabled(cx, data.connect.can_open_pairing(crate::model::PairSide::Join));
                    footer.draw_all(cx, &mut Scope::empty());
                    continue;
                }
                let Some(line) = lines.get(i) else { continue };
                let row = list.item(cx, i, if matches!(line.node, Node::Town(_)) { id!(TownRow) } else { id!(Row) });
                let open = row.button(cx, ids!(open));
                open.set_action_data(line.node);
                if matches!(line.node, Node::Live(Live::Share(_))) { self.share_buttons.push(open.widget_uid()); }
                crate::lists::open_label(cx, &row, ids!(name), &line.label);
                if matches!(line.node, Node::Town(_)) {
                    row.label(cx, ids!(note)).set_text(cx, &line.note);
                    let badge = row.view(cx, ids!(unread_badge));
                    let shown = line.unread > 0;
                    if badge.visible() != shown { badge.set_visible(cx, shown); }
                    if shown {
                        let count = row.label(cx, ids!(unread_badge.count));
                        let text = if line.unread > 99 { "99+".to_owned() } else { line.unread.to_string() };
                        if count.text() != text { count.set_text(cx, &text); }
                        let (bg, fg) = if line.urgent { (self.badge_urgent_bg, self.badge_urgent_text) } else { (self.badge_bg, self.badge_text) };
                        if let Some(mut view) = badge.borrow_mut() {
                            view.draw_bg.set_dyn_instance(cx, id!(color), &[bg.x, bg.y, bg.z, bg.w]);
                        }
                        count.set_text_color(cx, fg);
                    }
                }
                row.label(cx, ids!(glyph)).set_text(cx, line.glyph);
                let twisty = row.button(cx, ids!(twisty));
                twisty.set_visible(cx, line.fold.is_some());
                row.widget(cx, ids!(twisty_gap)).set_visible(cx, line.fold.is_none());
                if let Some(expanded) = line.fold { twisty.set_text(cx, if expanded { "▾" } else { "▸" }); }
                if let Some(mut indent) = row.view(cx, ids!(indent)).borrow_mut() {
                    indent.walk.width = Size::Fixed(14.0 * line.depth as f64);
                }
                highlight(cx, &row, ids!(mark), ids!(open), line.open);
                row.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tree::{TreeProject, TreeRow};

    fn id(n: u8) -> Id { [n; 32] }

    fn state() -> (TreeState, Id, Id) {
        let (town, other) = (id(1), id(2));
        let project = |n: u8, title: &str| TreeProject {
            id: id(n), title: title.into(), tasks: 0,
            rows: vec![
                TreeRow { id: id(n), kind: RowKind::Tasks, title: "Tasks".into() },
                TreeRow { id: id(n + 10), kind: RowKind::Board, title: "Layout".into() },
                TreeRow { id: id(n + 20), kind: RowKind::Doc, title: "Notes".into() },
            ],
        };
        let tree = TownTree { town: Some(town), name: Some("Makepad".into()), label: "Makepad".into(), projects: vec![project(30, "Fab"), project(31, "Town")] };
        let mut state = TreeState::default();
        state.set(vec![TownEntry { id: town, title: "Makepad".into(), note: "with Ben".into() },
            TownEntry { id: other, title: "Studio".into(), note: "only here".into() }], tree);
        (state, town, other)
    }

    #[test]
    fn rows_fold_only_by_their_twisty_and_names_open_by_id() {
        let (mut state, town, other) = state();
        fn labels(s: &TreeState) -> Vec<(usize, &str)> { s.lines.iter().map(|l| (l.depth, l.label.as_str())).collect() }
        assert_eq!(labels(&state), [(0, "Makepad"), (1, "Fab"), (2, "Tasks"), (2, "Issues"), (2, "Layout"), (2, "Notes"),
            (1, "Town"), (2, "Tasks"), (2, "Issues"), (2, "Layout"), (2, "Notes"), (0, "Studio")], "the open town shows each project's rows");
        assert_eq!(state.lines[0].node, Node::Town(town), "the town row carries the town's id");
        assert_eq!(state.lines[2].node, Node::Tasks(id(30)), "a task list row carries its project's id");
        assert_eq!(state.lines[3].node, Node::Issues(id(30)), "Issues opens the same project as the adjacent Tasks row");
        assert_eq!(state.lines[4].node, Node::Board(id(40)), "a board row carries the board's id");
        assert_eq!(state.lines[11].fold, Some(false), "a town that isn't open is collapsed");

        let plain = state.lines.clone();
        let session = [8; 16];
        for (role, expected) in [
            (ShareRole::Viewer { sharer: [2; 65] }, "Ada is sharing Drawing"),
            (ShareRole::Sharer { link: crate::app_link::LinkId::TEST, window: crate::app_link::WindowId(1) }, "You are sharing Drawing"),
        ] {
            state.set_live(vec![LiveRow::share(session, role, "Ada", "Drawing")]);
            let row = &state.lines[1];
            assert_eq!((row.node, row.depth, row.label.as_str(), row.fold), (Node::Live(Live::Share(session)), 1, expected, None));
            assert_eq!(state.lines[0].node, Node::Town(town));
            assert_eq!(state.press(1, Press::Name), Some(Node::Live(Live::Share(session))));
        }
        assert_eq!(state.lines[1].node, Node::Live(Live::Share(session)), "the share precedes the projects in the wide rail");
        assert_eq!(state.lines[2].node, Node::Project(id(30)));
        let mut call = CallView { peer_name: "Ada".into(), ..Default::default() };
        let share = LiveRow::share(session, ShareRole::Viewer { sharer: [2; 65] }, "Ada", "Drawing");
        for (phase, secs, label) in [
            (Phase::Calling, 0, "Calling Ada…"), (Phase::Ringing, 0, "Ada is calling"),
            (Phase::Live, 59, "Call with Ada · 0:59"), (Phase::Live, 60, "Call with Ada · 1:00"),
            (Phase::Live, 3661, "Call with Ada · 61:01"),
        ] {
            call.phase = phase;
            call.duration_secs = secs;
            let before = state.lines.as_ptr();
            let rebuilt = state.set_live(vec![LiveRow::call(&call).unwrap(), share.clone()]);
            assert_eq!(rebuilt, phase == Phase::Calling, "only inserting the call changes the tree layout");
            if !rebuilt { assert_eq!(before, state.lines.as_ptr(), "the existing rows survive a clock tick"); }
            let row = &state.lines[1];
            assert_eq!((row.node, row.depth, row.label.as_str(), row.fold), (Node::Live(Live::Call), 1, label, None));
            assert_eq!(state.lines[2].node, Node::Live(Live::Share(session)));
            assert_eq!(state.press(1, Press::Name), Some(Node::Live(Live::Call)));
        }
        call.phase = Phase::Idle;
        assert!(LiveRow::call(&call).is_none());
        state.set_live(vec![share]);
        assert_eq!(state.lines[1].node, Node::Live(Live::Share(session)), "ending the call keeps the share");
        state.set_live(Vec::new());
        assert_eq!(state.lines, plain, "ending the offer restores the original rows");

        let before = state.lines.clone();
        assert_eq!(state.press(4, Press::Name), Some(Node::Board(id(40))), "a name opens its row's id");
        assert_eq!(state.lines, before, "a name folds nothing");
        assert_eq!(state.press(1, Press::Name), Some(Node::Project(id(30))), "a project's name opens its overview");
        assert_eq!(state.lines, before, "a project's name folds nothing");

        assert_eq!(state.press(1, Press::Twisty), None, "a twisty opens nothing");
        assert_eq!(labels(&state), [(0, "Makepad"), (1, "Fab"),
            (1, "Town"), (2, "Tasks"), (2, "Issues"), (2, "Layout"), (2, "Notes"), (0, "Studio")], "the twisty folds only its project");
        assert_eq!(state.lines[1].fold, Some(false), "the folded project shows ▸");
        state.set(state.towns.clone(), state.snapshot().clone());
        assert_eq!(state.lines.len(), 8, "a refresh keeps this computer's folds");
        assert_eq!(state.press(1, Press::Twisty), None);
        assert_eq!(state.lines, before, "the twisty unfolds it again");

        assert_eq!(state.press(11, Press::Twisty), Some(Node::Town(other)), "another town's twisty opens that town");
        assert_eq!(state.press(2, Press::Twisty), None, "a leaf has no twisty");
        state.show(Some(Node::Board(id(40))));
        assert_eq!(state.lines.iter().filter(|l| l.open).map(|l| l.node).collect::<Vec<_>>(), [Node::Board(id(40))],
            "the open page's row alone is marked");

        // Unread counts edit the town rows in place and survive a twisty's rebuild.
        let rows = state.lines.as_ptr();
        let counts = BTreeMap::from([(town, TownUnread { count: 2, ..Default::default() }),
            (other, TownUnread { count: 3, mentions_or_approvals: 1, ..Default::default() })]);
        fn badges(s: &TreeState) -> Vec<(Node, usize, bool)> {
            s.lines.iter().filter(|l| matches!(l.node, Node::Town(_))).map(|l| (l.node, l.unread, l.urgent)).collect()
        }
        assert!(state.set_unread(&counts), "new counts change the town rows");
        assert_eq!(state.lines.as_ptr(), rows, "counts edit the rows in place");
        assert_eq!(badges(&state), [(Node::Town(town), 2, false), (Node::Town(other), 3, true)], "a reply or @ makes a count urgent");
        assert!(state.lines.iter().filter(|l| !matches!(l.node, Node::Town(_))).all(|l| (l.unread, l.urgent) == (0, false)),
            "only town rows carry counts");
        assert!(!state.set_unread(&counts), "the same counts change nothing");
        assert_eq!(state.press(1, Press::Twisty), None);
        assert_eq!((state.lines[7].node, state.lines[7].unread, state.lines[7].urgent), (Node::Town(other), 3, true),
            "folding a project keeps the counts");
        assert_eq!(state.press(1, Press::Twisty), None);
        assert_eq!((state.lines[11].node, state.lines[11].unread, state.lines[11].urgent), (Node::Town(other), 3, true),
            "unfolding keeps them too");
        assert_eq!(badges(&state)[0], (Node::Town(town), 2, false));
        assert!(state.set_unread(&BTreeMap::new()), "clearing the counts changes the rows");
        assert_eq!(badges(&state), [(Node::Town(town), 0, false), (Node::Town(other), 0, false)], "no counts, no badges");
    }
}
