//! Visible rows for the task columns and chat, and what the shell's pages show.

use makepad_widgets::*;
use crate::model::{Model, TownUnread};
use makepad_town_core::event::Id;
use std::collections::{BTreeMap, BTreeSet};

pub use crate::ui::chat::{composer_emoji_actions, load_recent_emoji, mention_hint_text, ChatCommand};
pub use crate::ui::home::{home_grid, project_grid, summary, GridRow, HomeAction, NewKind, Target};
pub use crate::ui::town_tree::{Live, Node, Press};
pub use crate::ui::talk::{compact_forms, form_for_width, parked_form, Form, Pane};
pub use crate::ui::tasks::{TaskBoardWidgetRefExt, TaskEdits};
use crate::ui::tasks::{TaskCardBody, TaskMode};

/// What the lists draw.
#[derive(Default)]
pub struct ViewData {
    pub call: crate::call::CallShown,
    pub issues: crate::ui::issues::Issues,
    pub share_popout: SharePopout,
    pub share_peer: Option<makepad_town_core::key::PublicKey>,
    /// The closed app whose notice is still open in the center.
    pub open_ended_share: Option<makepad_town_core::event::SessionId>,
    pub member_revision: u64,
    pub pair_problem: Option<String>,
    pub members: crate::view::TownMembers,
    pub selected_member: Option<crate::view::MemberId>,
    pub workspace: crate::view::Workspace,
    pub(crate) task_mode: TaskMode,
    pub expanded_transcripts: std::collections::BTreeSet<crate::view::TranscriptId>,
    pub page: Page,
    pub form: Form,
    pub(crate) tabs_as_icons: bool,
    pub pane: Pane,
    pub(crate) share_chat_open: bool,
    /// Why the selected message's last action failed, shown in its bar.
    pub chat_error: Option<(makepad_town_core::event::Id, String)>,
    /// The selected message or picture in the transcript.
    pub chat_selected: Option<makepad_town_core::event::Id>,
    /// The message the composer will reply to in the open town.
    pub reply_to: Option<makepad_town_core::event::Id>,
    /// What this computer has read of each town's chat, and each town's unread count.
    pub chat_read: ChatReadState,
    pub town_ids: Vec<makepad_town_core::event::Id>,
    pub town_index: usize,
    pub share_error: Option<String>,
    pub publish: Option<crate::publish::session::LocalPublish>,
    pub open_doc: Option<makepad_town_core::event::Id>,
    pub open_board: Option<makepad_town_core::event::Id>,
    pub delete: Delete,
    pub tree: crate::ui::town_tree::TreeState,
    pub home_grid: Vec<crate::ui::home::GridRow>,
    pub overview_grid: Vec<crate::ui::home::GridRow>,
    pub connect: crate::link_worker::ConnectView,
    pub retry_quiet: crate::view::RetryQuiet,
}

/// Keep the native window bound to its session until its close is acknowledged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SharePopout {
    #[default]
    Closed,
    Open(makepad_town_core::event::SessionId),
    Closing(makepad_town_core::event::SessionId),
}

impl SharePopout {
    pub fn session(self) -> Option<makepad_town_core::event::SessionId> {
        match self { Self::Closed => None, Self::Open(id) | Self::Closing(id) => Some(id) }
    }
}

/// What this computer has read of one town's chat.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct TownRead {
    /// The newest line read; `None` when the town was seeded with no chat.
    pub seen: Option<Id>,
    /// The first line that arrived unread, kept for the chat's divider.
    pub divider_before: Option<Id>,
}

/// Each town's read line and unread count, kept in memory on this computer.
#[derive(Debug)]
pub struct ChatReadState {
    pub towns: BTreeMap<Id, TownRead>,
    pub counts: BTreeMap<Id, TownUnread>,
    /// The open town's chat shows its newest line. Stays true until the chat reports its scroll.
    pub open_at_tail: bool,
    /// The newest line seen while no town is open.
    unscoped_seen: Option<Id>,
}

impl Default for ChatReadState {
    fn default() -> Self {
        Self { towns: BTreeMap::new(), counts: BTreeMap::new(), open_at_tail: true, unscoped_seen: None }
    }
}

impl ChatReadState {
    /// Every town this computer holds starts read at its newest line, so startup counts nothing.
    pub fn seed_loaded(&mut self, model: &Model) {
        for entry in model.towns() {
            self.towns.entry(entry.id).or_insert_with(|| TownRead { seen: model.town_chat_tip(entry.id), divider_before: None });
        }
    }

    /// Walk the unread suffix of each loaded town. Closed towns keep their cached counts.
    pub fn sync_counts(&mut self, model: &Model, town_ids: &[Id], open_visible: bool) {
        let town_ids: BTreeSet<_> = town_ids.iter().copied().collect();
        self.counts.retain(|id, _| town_ids.contains(id));
        let open = model.open_id();
        for id in town_ids {
            let read = self.towns.entry(id).or_insert_with(|| TownRead { seen: model.town_chat_tip(id), divider_before: None });
            let Some(mut unread) = model.town_unread(id, read.seen) else { continue };
            if id == open && open_visible && self.open_at_tail {
                read.seen = unread.newest;
                unread = TownUnread { newest: unread.newest, ..TownUnread::default() };
            } else if unread.count > 0 && read.divider_before.is_none() {
                read.divider_before = unread.first_unread;
            }
            self.counts.insert(id, unread);
        }
    }

    /// The open town's chat is on screen at `newest`: all of it is read.
    pub fn mark_open_read(&mut self, open: Id, newest: Option<Id>) {
        self.towns.entry(open).or_default().seen = newest;
        if let Some(unread) = self.counts.get_mut(&open) {
            *unread = TownUnread { newest, ..TownUnread::default() };
        }
    }

    pub fn open_unread(&self, open: Id) -> TownUnread {
        self.counts.get(&open).copied().unwrap_or_default()
    }

    pub fn any_background_unread(&self, open: Id) -> bool {
        self.counts.iter().any(|(&id, unread)| id != open && unread.count > 0)
    }

    pub(crate) fn mark_unscoped_read(&mut self, newest: Option<Id>) { self.unscoped_seen = newest; }

    pub(crate) fn unscoped_seen(&self) -> Option<Id> { self.unscoped_seen }
}

/// Where a board's or doc's Delete stands: the ⋯ menu open, or its one confirmation asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delete {
    #[default]
    Closed,
    Menu(makepad_town_core::event::Id),
    Confirm(makepad_town_core::event::Id),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    /// The open town's Home.
    #[default]
    Home,
    /// A project's overview.
    Project,
    Tasks,
    Issues,
    Board,
    Docs,
    Live(Live),
    Publish,
    /// Joining a town on another computer: a page of its own that shows nothing of the open town.
    Join,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.TownTreeListBase = #(crate::ui::town_tree::TownTreeList::register_widget(vm))
    mod.widgets.HomeMembersBase = #(crate::ui::home::HomeMembers::register_widget(vm))
    mod.widgets.TileGridBase = #(crate::ui::home::TileGrid::register_widget(vm))
    mod.widgets.CreateTaskListBase = #(CreateTaskList::register_widget(vm))
}

#[derive(Script, ScriptHook, Widget)]
pub struct CreateTaskList {
    #[live]
    column: usize,
    #[rust]
    omitted: Option<makepad_town_core::event::Id>,
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl CreateTaskList {
    pub(crate) fn omitted(&self) -> Option<makepad_town_core::event::Id> { self.omitted }
}

impl Widget for CreateTaskList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<ReorderList>() else { continue };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let Some(project) = data.workspace.selected() else { list.set_item_range(cx, 0, 0); continue };
            let Some(cards) = project.columns.get(self.column) else { continue };
            let hidden = data.task_mode.source_slot(project.id, self.column).filter(|&index| index < cards.len());
            self.omitted = hidden.and_then(|index| cards.get(index)).map(|c| c.id);
            list.set_item_range(cx, 0, cards.len() - usize::from(hidden.is_some()));
            while let Some(index) = list.next_visible_item(cx) {
                let at = index + usize::from(hidden.is_some_and(|i| index >= i));
                let Some(card) = cards.get(at).filter(|_| index < list.range_end()) else { continue };
                let item = list.item(cx, index, id!(TaskRow));
                if let Some(mut row) = item.borrow_mut::<TaskCardBody>() {
                    row.bind(cx, project.id, self.column, at, card, data);
                }
                item.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

/// Bind the visible title and its button together on the actual list item, including recycled rows.
pub(crate) fn open_label(cx: &mut Cx, row: &WidgetRef, path: &[LiveId], text: &str) {
    let label = row.label(cx, path);
    if label.text() == text { return }
    label.set_text(cx, text);
    let mut button = row.button(cx, ids!(open));
    let name = cx.with_vm(|vm| vm.new_string(text));
    script_apply_eval!(cx, button, {a11y_name: #(name)});
}

pub(crate) fn highlight(cx: &mut Cx, row: &WidgetRef, mark: &[LiveId], button: &[LiveId], selected: bool) {
    let marker = row.widget(cx, mark);
    if marker.visible() == selected {
        return;
    }
    marker.set_visible(cx, selected);
    let mut face = row.button(cx, button);
    script_apply_eval!(cx, face, {draw_bg.color: if #(selected) {mod.palette.sel} else {mod.palette.panel}});
}

pub(crate) fn seat_icon(cx: &mut Cx, icons: &WidgetRef, shown: crate::view::Seat, name: &str) {
    use crate::view::Seat::*;
    let path = match shown {
        You => ids!(you),
        YourAi => ids!(your_ai),
        Peer => ids!(peer),
        PeerAi => ids!(peer_ai),
    };
    if !icons.widget(cx, path).visible() {
        for child in [ids!(you), ids!(your_ai), ids!(peer), ids!(peer_ai)] {
            icons.widget(cx, child).set_visible(cx, child == path);
        }
    }
    if shown == Peer {
        let label = icons.label(cx, ids!(peer.initial));
        let initial = crate::view::peer_initial(name);
        if label.text() != initial { label.set_text(cx, &initial); }
    }
}

impl ViewData {
    pub fn narrow(&self) -> bool { self.form == Form::Narrow }

    /// The newest chat message.
    pub fn newest_message(&self) -> Option<makepad_town_core::event::Id> {
        self.workspace.rows.iter().rev().find_map(|row| match row {
            crate::view::ChatRow::Message(m) => Some(m.id),
            _ => None,
        })
    }

    /// A message arrived while another pane was shown.
    pub fn chat_unread(&self) -> bool {
        match self.tree.snapshot().town {
            Some(open) => self.chat_read.open_unread(open).count > 0,
            None => self.newest_message() != self.chat_read.unscoped_seen(),
        }
    }

    /// The pane to show in the narrow form.
    pub fn show_pane(&mut self, pane: Pane) {
        self.pane = pane;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::{exchange, shared_town};

    #[test]
    fn read_state_seeds_closed_towns_and_keeps_counts_when_released() {
        let (_homes, mut a, mut b) = shared_town("readstate");
        let town = a.open_id();
        let before = a.post_chat_draft("before", None, None, None).unwrap();
        exchange(&mut a, &mut b);
        let ids: Vec<Id> = b.towns().iter().map(|t| t.id).collect();
        let own = ids.iter().copied().find(|id| *id != town).unwrap();
        b.open_town(own).unwrap();
        b.sync_stop();
        assert!(b.loaded_town(town).is_none(), "the shared town is closed on this computer");

        let mut read = ChatReadState::default();
        read.seed_loaded(&b);
        assert_eq!(read.towns[&town].seen, Some(before), "a closed town seeds at its summary's newest line");
        read.sync_counts(&b, &ids, false);
        assert_eq!(read.open_unread(town), TownUnread::default(), "startup counts nothing in a closed town");
        assert!(!read.any_background_unread(own), "nothing waits at startup");

        a.sync_stop();
        a.sync_start(&b.me());
        b.sync_start(&a.me());
        let after = a.post_chat_draft("after", None, None, None).unwrap();
        exchange(&mut a, &mut b);
        assert_eq!(b.open_id(), own);
        read.sync_counts(&b, &ids, false);
        let counted = TownUnread { count: 1, mentions_or_approvals: 0, first_unread: Some(after), newest: Some(after) };
        assert_eq!(read.open_unread(town), counted, "only the line after the seeded tip counts");
        assert_eq!(read.towns[&town].divider_before, Some(after), "the first unread line marks the divider");
        assert!(read.any_background_unread(own));

        b.sync_stop();
        assert!(b.loaded_town(town).is_none(), "the dropped link releases the town");
        read.sync_counts(&b, &ids, false);
        assert_eq!(read.open_unread(town), counted, "a released town keeps its count");

        b.open_town(town).unwrap();
        read.sync_counts(&b, &ids, false);
        assert_eq!(read.open_unread(town).count, 1, "opening the town with chat hidden keeps it unread");
        read.sync_counts(&b, &ids, true);
        assert_eq!(read.open_unread(town), TownUnread { newest: Some(after), ..TownUnread::default() }, "the visible open town is read");
        assert_eq!(read.towns[&town].seen, Some(after));

        read.sync_counts(&b, &[town], true);
        assert!(!read.counts.contains_key(&own), "a town that left the rail drops its count");
    }
}
