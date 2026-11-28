//! Visible rows for the task columns and chat, and what the shell's pages show.

use makepad_widgets::*;
use crate::model::{Model, TownUnread};
use crate::view::{Seat, SeatTone};
use makepad_town_core::event::Id;
use std::collections::{BTreeMap, BTreeSet};

pub use crate::theme::SeatPalette;
pub use crate::ui::chat::{composer_emoji_actions, load_recent_emoji, mention_hint_text, ChatCommand, MentionPick, MentionPickerState};
pub use crate::ui::home::{clear_member_tip, device_item_id, home_grid, project_grid, summary, GridRow, HomeAction, MemberDeviceRow, NewKind, Target};
pub use crate::ui::town_tree::{Live, Node, Press};
pub use crate::ui::talk::{compact_forms, form_for_width, parked_form, Form, Pane};
pub use crate::ui::tasks::{TaskBoardWidgetRefExt, TaskEdits};
use crate::ui::tasks::{TaskCardBody, TaskMode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveTownCall {
    pub town: makepad_town_core::event::TownId,
    pub session: makepad_town_core::event::SessionId,
    pub scope: crate::call::CallScope,
    pub participants: Vec<String>,
    pub can_join: bool,
}

/// What the lists draw.
#[derive(Default)]
pub struct ViewData {
    pub mention_picker: MentionPickerState,
    pub call: crate::call::CallShown,
    pub active_town_call: Option<ActiveTownCall>,
    pub active_town_call_revision: u64,
    pub live_roster: crate::view::LiveRoster,
    pub issues: crate::ui::issues::Issues,
    pub share_popout: SharePopout,
    /// The closed app whose notice is still open in the center.
    pub open_ended_share: Option<makepad_town_core::event::SessionId>,
    pub member_revision: u64,
    pub pair_problem: Option<String>,
    pub members: crate::view::TownMembers,
    pub seat_palette: SeatPalette,
    pub member_devices: Vec<MemberDeviceRow>,
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
    /// Message briefly outlined after a reply-quote jump.
    pub chat_flash: Option<makepad_town_core::event::Id>,
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
    pub tree_menu: Option<Node>,
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
    pub(crate) rendered_rows: Vec<crate::view::ChatRowKey>,
    pub(crate) rendered_rev: u64,
    pub(crate) rendered_town: Option<Id>,
    pub(crate) pending_cue_shown: Option<(Option<Id>, Id)>,
    pub(crate) pending_cue_rev: u64,
    pub towns: BTreeMap<Id, TownRead>,
    pub counts: BTreeMap<Id, TownUnread>,
    /// Drops as soon as the reader scrolls up and re-arms on returning to the tail.
    pub open_at_tail: bool,
    user_scrolled_up: bool,
    /// The newest line seen while no town is open.
    unscoped_seen: Option<Id>,
}

impl Default for ChatReadState {
    fn default() -> Self {
        Self { rendered_rows: Vec::new(), rendered_rev: 0, rendered_town: None, pending_cue_shown: None, pending_cue_rev: 0,
            towns: BTreeMap::new(), counts: BTreeMap::new(), open_at_tail: true, user_scrolled_up: false, unscoped_seen: None }
    }
}

impl ChatReadState {
    pub(crate) fn set_pending_cue_shown(&mut self, next: Option<(Option<Id>, Id)>) {
        if self.pending_cue_shown != next {
            self.pending_cue_shown = next;
            self.pending_cue_rev = self.pending_cue_rev.wrapping_add(1);
        }
    }

    /// Forget the chat rows when switching towns.
    pub fn reset_rendered_rows(&mut self) {
        self.rendered_rows.clear();
        self.rendered_rev = 0;
        self.rendered_town = None;
        self.set_pending_cue_shown(None);
    }

    /// Whether the open chat is following its newest line at the live tail.
    pub fn at_live_tail(&self) -> bool {
        self.open_at_tail && !self.user_scrolled_up
    }

    /// Whether the user has actively scrolled up away from the tail.
    pub fn user_scrolled_up(&self) -> bool {
        self.user_scrolled_up
    }

    pub fn mark_user_scrolled_up(&mut self) {
        self.user_scrolled_up = true;
        self.open_at_tail = false;
    }

    pub fn mark_at_tail(&mut self) {
        self.open_at_tail = true;
        self.user_scrolled_up = false;
    }

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
        let at_live_tail = self.at_live_tail();
        for id in town_ids {
            let read = self.towns.entry(id).or_insert_with(|| TownRead {
                seen: model.town_chat_tip(id),
                divider_before: None,
            });
            let Some(mut unread) = model.town_unread(id, read.seen) else { continue };
            let anchor_missing = |anchor: Id| {
                model.loaded_town(id).is_some_and(|t| t.state.message(&anchor).is_none())
            };
            if id == open && open_visible && at_live_tail {
                if read.divider_before.is_some_and(anchor_missing) {
                    read.divider_before = unread.first_unread;
                }
                read.seen = unread.newest;
                unread = TownUnread { newest: unread.newest, ..TownUnread::default() };
            } else if unread.count > 0 {
                read.divider_before = unread.first_unread;
            } else if read.divider_before.is_some_and(anchor_missing) {
                read.divider_before = None;
            }
            self.counts.insert(id, unread);
        }
    }

    /// The open town's chat is on screen at `newest`: advance `seen` and zero counts while
    /// keeping `divider_before` sticky (used by `show_panes` and manual scroll-to-tail).
    pub fn mark_open_seen(&mut self, open: Id, newest: Option<Id>) {
        self.towns.entry(open).or_default().seen = newest;
        if let Some(unread) = self.counts.get_mut(&open) {
            *unread = TownUnread { newest, ..TownUnread::default() };
        }
    }

    /// Explicit read (send or jump-to-latest): advance `seen`, zero counts, and clear `divider_before`.
    pub fn mark_open_read(&mut self, open: Id, newest: Option<Id>) {
        let read = self.towns.entry(open).or_default();
        read.seen = newest;
        read.divider_before = None;
        if let Some(unread) = self.counts.get_mut(&open) {
            *unread = TownUnread { newest, ..TownUnread::default() };
        }
    }

    /// Clear a sticky divider when leaving a town that has no remaining unread messages.
    pub fn clear_divider_if_read(&mut self, town: Id) {
        if self.open_unread(town).count == 0 {
            if let Some(read) = self.towns.get_mut(&town) {
                read.divider_before = None;
            }
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
    mod.widgets.SeatIconsBase = #(SeatIcons::register_widget(vm))
    mod.widgets.SeatDevicesBase = #(crate::ui::home::SeatDevices::register_widget(vm))
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

#[derive(Script, Widget)]
pub struct SeatIcons {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[live] seat_0: Vec4f,
    #[live] seat_1: Vec4f,
    #[live] seat_2: Vec4f,
    #[live] seat_3: Vec4f,
    #[live] seat_4: Vec4f,
    #[live] seat_5: Vec4f,
    #[live] seat_6: Vec4f,
    #[live] seat_7: Vec4f,
    #[live] peer_default: Vec4f,
    #[live] peer_ai_default: Vec4f,
    #[live] ink_dark: Vec4f,
    #[live] ink_light: Vec4f,
    #[rust] painted: Option<(Seat, Option<SeatTone>)>,
    #[cfg(test)]
    #[rust] peer_paints: usize,
    #[cfg(test)]
    #[rust] initial_writes: usize,
}

#[cfg(test)]
impl SeatIcons {
    pub(crate) fn paint_counts(&self) -> (usize, usize) { (self.peer_paints, self.initial_writes) }
}

impl ScriptHook for SeatIcons {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() { self.painted = None; }
    }
}

impl Widget for SeatIcons {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

pub fn seat_icon(cx: &mut Cx, icons: &WidgetRef, shown: Seat, name: &str, tone: Option<SeatTone>) {
    use Seat::*;
    let Some(mut icons) = icons.borrow_mut::<SeatIcons>() else { return };
    let path = match shown {
        You => ids!(you),
        YourAi => ids!(your_ai),
        Peer => ids!(peer),
        PeerAi => ids!(peer_ai),
    };
    if icons.painted != Some((shown, tone)) {
        let badge = icons.view.widget(cx, path);
        if !badge.visible() {
            for child in [ids!(you), ids!(your_ai), ids!(peer), ids!(peer_ai)] {
                icons.view.widget(cx, child).set_visible(cx, child == path);
            }
        }
        if matches!(shown, Peer | PeerAi) {
            let seats = [icons.seat_0, icons.seat_1, icons.seat_2, icons.seat_3,
                icons.seat_4, icons.seat_5, icons.seat_6, icons.seat_7];
            let fill = tone.and_then(|t| seats.get(t.0 as usize).copied())
                .unwrap_or(if shown == Peer { icons.peer_default } else { icons.peer_ai_default });
            let ink = crate::theme::contrast_ink(fill, icons.ink_dark, icons.ink_light);
            if let Some(mut view) = badge.borrow_mut::<View>() {
                view.draw_bg.set_dyn_instance(cx, id!(color), &[fill.x, fill.y, fill.z, fill.w]);
            }
            badge.label(cx, ids!(initial)).set_text_color(cx, ink);
            #[cfg(test)]
            { icons.peer_paints += 1; }
        }
        icons.painted = Some((shown, tone));
    }
    if matches!(shown, You | Peer) {
        let (label_path, fallback) = if shown == You { (ids!(you.initial), 'Y') } else { (ids!(peer.initial), 'P') };
        let label = icons.view.label(cx, label_path);
        let initial = crate::view::seat_initial(name, fallback);
        if label.text() != initial {
            label.set_text(cx, &initial);
            #[cfg(test)]
            { icons.initial_writes += 1; }
        }
    }
}

pub fn paint_peer_badge(cx: &mut Cx, badge: &WidgetRef, tone: Option<SeatTone>, palette: &SeatPalette) {
    if badge.is_empty() { return }
    let (fill, ink) = match tone {
        Some(SeatTone(slot)) => {
            let fill = palette.seats[slot as usize % crate::view::SEAT_SLOTS];
            (fill, crate::theme::contrast_ink(fill, palette.ink_dark, palette.ink_light))
        }
        None => cx.with_vm(|vm| {
            let palette = vm.module(id!(palette));
            let fill = vm.value(palette, id!(peer));
            let ink = vm.value(palette, id!(onpeer));
            (Vec4f::script_from_value(vm, fill), Vec4f::script_from_value(vm, ink))
        }),
    };
    if let Some(mut view) = badge.borrow_mut::<View>() {
        view.draw_bg.set_dyn_instance(cx, id!(color), &[fill.x, fill.y, fill.z, fill.w]);
    }
    badge.label(cx, ids!(initial)).set_text_color(cx, ink);
    let mut ring = badge.widget(cx, ids!(ring));
    if !ring.is_empty() {
        script_apply_eval!(cx, ring, {draw_bg.ink: #(ink)});
    }
    badge.redraw(cx);
}

#[cfg(test)]
pub(crate) fn badge_colors(cx: &Cx, badge: &WidgetRef) -> (Vec4f, Vec4f) {
    let view = badge.borrow::<View>().unwrap();
    let vars = &view.draw_bg.draw_vars;
    let color = vars.dyn_instance_inputs(cx).iter().find(|input| input.id == id!(color)).unwrap();
    let at = vars.dyn_instance_start + color.offset;
    let fill = &vars.dyn_instances[at..at + 4];
    let ink = badge.label(cx, ids!(initial)).borrow().unwrap().draw_text.color;
    (vec4(fill[0], fill[1], fill[2], fill[3]), ink)
}

impl ViewData {
    pub fn set_active_town_call(&mut self, next: Option<ActiveTownCall>) -> bool {
        if self.active_town_call == next { return false }
        self.active_town_call = next;
        self.active_town_call_revision = self.active_town_call_revision.wrapping_add(1);
        true
    }

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

        read.mark_open_seen(town, Some(after));
        assert_eq!(read.towns[&town].divider_before, Some(after), "reading at the tail keeps the divider");
        read.clear_divider_if_read(town);
        assert_eq!(read.towns[&town].divider_before, None, "leaving a read town clears its divider");

        a.sync_start(&b.me());
        b.sync_start(&a.me());
        let live = a.post_chat_draft("at the tail", None, None, None).unwrap();
        exchange(&mut a, &mut b);
        read.sync_counts(&b, &ids, true);
        assert_eq!(read.towns[&town], TownRead { seen: Some(live), divider_before: None },
            "a visible live-tail arrival is read without a divider");
        read.mark_user_scrolled_up();
        let first = a.post_chat_draft("first batch", None, None, None).unwrap();
        exchange(&mut a, &mut b);
        read.sync_counts(&b, &ids, true);
        read.clear_divider_if_read(town);
        assert_eq!(read.towns[&town].divider_before, Some(first), "leaving an unread town keeps its divider");
        read.mark_open_seen(town, Some(first));
        let msg_b = a.post_chat_draft("next batch", None, None, None).unwrap();
        let msg_c = a.post_chat_draft("last in batch", None, None, None).unwrap();
        exchange(&mut a, &mut b);
        read.sync_counts(&b, &ids, true);
        assert_eq!(read.open_unread(town).count, 2);
        assert_eq!(read.towns[&town].divider_before, Some(msg_b), "a new unread batch replaces the sticky divider");
        let shows_c = |model: &Model| crate::view::Workspace::read(model, false, &Default::default()).rows.iter()
            .any(|row| matches!(row, crate::view::ChatRow::Message(m) if m.id == msg_c));
        a.delete_message(msg_b).unwrap();
        exchange(&mut a, &mut b);
        read.sync_counts(&b, &ids, true);
        assert_eq!((read.open_unread(town).count, read.open_unread(town).first_unread, read.towns[&town].divider_before),
            (1, Some(msg_c), Some(msg_c)), "the divider follows the surviving unread message");
        assert!(shows_c(&b));
        a.delete_message(msg_c).unwrap();
        exchange(&mut a, &mut b);
        read.sync_counts(&b, &ids, true);
        assert_eq!((read.open_unread(town).count, read.towns[&town].divider_before), (0, None));
        assert!(!shows_c(&b), "retracting the last unread message removes its row and divider");

        let marked = a.post_chat_draft("marked read directly", None, None, None).unwrap();
        exchange(&mut a, &mut b);
        read.sync_counts(&b, &ids, true);
        assert_eq!(read.open_unread(town).count, 1);
        assert_eq!(read.towns[&town].divider_before, Some(marked));
        read.mark_open_read(town, Some(marked));
        assert_eq!(read.open_unread(town).count, 0);
        assert_eq!(read.towns[&town].seen, Some(marked));
        assert_eq!(read.towns[&town].divider_before, None);

    }
}
