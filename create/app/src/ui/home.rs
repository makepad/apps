//! The town's Home and a project's overview: titles, a meta line, and sections of tiles.
//! Every tile carries the id of what it opens; its title is display text only.

use crate::view::{LiveActivity, MemberId, MemberSeat, Presence};
use crate::lists::ViewData;
use crate::model::tree::TownTree;
use crate::ui::town_tree::Node;
use makepad_widgets::{tip::TipWidgetRefExt, *};
use makepad_town_core::{event::Id, key::PublicKey};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    Project,
    Board,
    Doc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Open(Node),
    New(NewKind),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub target: Target,
    pub title: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GridRow {
    Section(String),
    Tiles(Vec<Tile>),
}

fn count(n: usize, one: &str) -> String { format!("{n} {one}{}", if n == 1 { "" } else { "s" }) }

fn new_tile(title: &str, kind: NewKind) -> Tile { Tile { target: Target::New(kind), title: title.into(), summary: String::new() } }

fn rows(section: &str, tiles: Vec<Tile>, columns: usize, grid: &mut Vec<GridRow>) {
    grid.push(GridRow::Section(section.into()));
    grid.extend(tiles.chunks(columns.max(1)).map(|row| GridRow::Tiles(row.to_vec())));
}

/// The meta line under a title.
pub fn summary(tasks: usize, boards: usize, docs: usize) -> String {
    [(tasks, "task"), (boards, "board"), (docs, "doc")].into_iter()
        .filter(|(n, _)| *n > 0).map(|(n, unit)| count(n, unit)).collect::<Vec<_>>().join(" · ")
}

/// The tiles of the town's Home: a Projects section, one tile per project, and a new-project tile.
pub fn home_grid(tree: &TownTree, columns: usize) -> Vec<GridRow> {
    let mut tiles: Vec<Tile> = tree.projects.iter().map(|p| {
        let count = |k| p.rows.iter().filter(|r| r.kind == k).count();
        Tile {
            target: Target::Open(Node::Project(p.id)),
            title: p.title.clone(),
            summary: summary(p.tasks, count(crate::model::tree::RowKind::Board), count(crate::model::tree::RowKind::Doc)),
        }
    }).collect();
    tiles.push(new_tile("＋ New project", NewKind::Project));
    let mut grid = Vec::new();
    rows("PROJECTS", tiles, columns, &mut grid);
    grid
}

/// The tiles of a project's overview: its boards, its docs, and Add to <project>.
pub fn project_grid(tree: &TownTree, project: Id, columns: usize) -> Vec<GridRow> {
    use crate::model::tree::RowKind;
    let mut grid = Vec::new();
    let Some(project) = tree.project(project) else { return grid };
    for (section, kind, glyph, node) in [
        ("BOARDS", RowKind::Board, "Board", Node::Board as fn(Id) -> Node),
        ("DOCS", RowKind::Doc, "Doc", Node::Doc as fn(Id) -> Node),
    ] {
        let tiles: Vec<Tile> = project.rows.iter().filter(|r| r.kind == kind)
            .map(|r| Tile { target: Target::Open(node(r.id)), title: r.title.clone(), summary: glyph.into() }).collect();
        if !tiles.is_empty() { rows(section, tiles, columns, &mut grid); }
    }
    let name = if project.title.is_empty() { "project" } else { project.title.as_str() };
    rows(&format!("ADD TO {}", name.to_uppercase()), vec![new_tile("＋ Board", NewKind::Board), new_tile("＋ Doc", NewKind::Doc)],
        columns, &mut grid);
    grid
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeAction {
    Join,
    AddAi(Id),
    Pair(Id, crate::model::AddComputer),
    AddMember(Id),
    Card { town: Id, member: MemberId },
    DeviceConnect { town: Id, member: MemberId, device: PublicKey },
    DeviceRemove { town: Id, member: MemberId, device: PublicKey },
}

pub(crate) fn member_item_id(id: MemberId) -> LiveId {
    let (tag, key): (u8, &[u8]) = match &id {
        MemberId::You => (0, &[]),
        MemberId::Seat(seat) => (1, seat),
        MemberId::YourAi => (2, &[]),
        MemberId::Agent(id) => (3, id),
    };
    let mut bytes = [0; 1 + std::mem::size_of::<Id>()];
    bytes[0] = tag;
    bytes[1..1 + key.len()].copy_from_slice(key);
    let id = LiveId::from_bytes(LiveId::SEED, &bytes, 0, 1 + key.len(), 0);
    if id == LiveId(0) || id == id!(member_tip) { LiveId(id.0 ^ 1) } else { id }
}

/// Dismiss a drawn chip's tip before the opening card's press delivers key focus.
pub fn clear_member_tip(cx: &mut Cx, home_members: &WidgetRef, member: MemberId) {
    let list = home_members.flat_list(cx, ids!(list));
    let Some(item) = list.borrow().and_then(|l| l.items.get(&member_item_id(member)).map(|i| i.widget.clone())) else { return };
    let tip = item.tip(cx, ids!(tip));
    if tip.borrow().is_some_and(|t| !t.text.is_empty()) {
        tip.set_text("");
        cx.widget_action(tip.widget_uid(), tip::TipAction::HoverOut);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberDeviceRow {
    pub key: PublicKey,
    pub name: String,
    pub detail: String,
    pub presence: Presence,
    pub can_connect: bool,
    pub can_remove: bool,
    pub is_me: bool,
}

pub fn device_item_id(key: &PublicKey) -> LiveId {
    let id = LiveId::from_bytes(LiveId::SEED, key, 0, key.len(), 0);
    if id == LiveId(0) { LiveId(1) } else { id }
}

#[cfg(test)]
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct MemberDrawStats {
    pub a11y_evals: usize,
    pub dot_updates: usize,
    pub bg_updates: usize,
    pub live_updates: usize,
}

#[derive(Script, Widget)]
pub struct HomeMembers {
    #[deref] view: View,
    #[live] live_call_color: Vec4f,
    #[live] live_share_color: Vec4f,
    #[rust] live_shown: HashMap<LiveId, bool>,
    #[rust] status_shown: HashMap<LiveId, String>,
    #[cfg(test)]
    #[rust] stats: MemberDrawStats,
}

impl ScriptHook for HomeMembers {
    fn on_after_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        // A reload re-applies each chip's template, resetting its mark to the call colour while the mark stays
        // visible: forget what was painted so the next draw writes each live mark's colour again.
        if apply.is_reload() {
            self.live_shown.clear();
        }
    }
}

fn status_kind(presence: Presence) -> StatusKind {
    match presence {
        Presence::Here => StatusKind::Success,
        Presence::Reaching | Presence::Working => StatusKind::InProgress,
        Presence::Away => StatusKind::Unknown,
    }
}

fn show_member(cx: &mut Cx, row: &WidgetRef, town: Id, seat: &MemberSeat, selected: bool,
    item_id: LiveId, live_shown: &mut HashMap<LiveId, bool>, status_shown: &mut HashMap<LiveId, String>,
    call_color: Vec4f, share_color: Vec4f,
    #[cfg(test)] stats: &mut MemberDrawStats,
) {
    if let Some(mut bg) = row.borrow_mut::<View>() {
        if bg.show_bg != selected {
            bg.show_bg = selected;
            #[cfg(test)]
            { stats.bg_updates += 1; }
        }
    }
    crate::lists::seat_icon(cx, &row.widget(cx, ids!(badge)), seat.seat, &seat.name, seat.tone);
    let want = seat.live.as_ref().map(|a| matches!(a, LiveActivity::Sharing { .. }));
    let mark = row.view(cx, ids!(live_mark));
    let visible = mark.visible();
    let prev = live_shown.get(&item_id).copied();
    if visible != want.is_some() || prev != want {
        if visible != want.is_some() { mark.set_visible(cx, want.is_some()); }
        if let Some(sharing) = want {
            let c = if sharing { share_color } else { call_color };
            if let Some(mut view) = mark.borrow_mut() {
                view.draw_bg.set_dyn_instance(cx, id!(color), &[c.x, c.y, c.z, c.w]);
            }
            live_shown.insert(item_id, sharing);
        } else {
            live_shown.remove(&item_id);
        }
        #[cfg(test)]
        { stats.live_updates += 1; }
    }
    let name = row.label(cx, ids!(name));
    let tip = row.tip(cx, ids!(tip));
    let mut select = row.button(cx, ids!(select));
    if name.text() != seat.name || status_shown.get(&item_id).map(String::as_str) != Some(seat.status.as_str()) {
        name.set_text(cx, &seat.name);
        seat.status.clone_into(status_shown.entry(item_id).or_default());
        let label = cx.with_vm(|vm| {
            if seat.status.is_empty() {
                vm.new_string(&seat.name)
            } else {
                vm.new_string(&format!("{}, {}", seat.name, seat.status))
            }
        });
        script_apply_eval!(cx, select, {a11y_name: #(label)});
        #[cfg(test)]
        { stats.a11y_evals += 1; }
    }
    tip.set_text(if selected { "" } else { &seat.status });
    let kind = status_kind(seat.presence);
    let dot = row.status_dot(cx, ids!(dot));
    if dot.status() != Some(kind) {
        dot.set_status(cx, kind);
        #[cfg(test)]
        { stats.dot_updates += 1; }
    }
    select.set_action_data(HomeAction::Card { town, member: seat.id });
}

impl Widget for HomeMembers {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        #[cfg(test)]
        { self.stats = MemberDrawStats::default(); }
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let Some(town) = data.tree.snapshot().town else { continue };
            let seats = &data.members.seats;
            let Some(mut list) = step.borrow_mut::<FlatList>() else { continue };
            let Some(add) = list.item(cx, id!(member_tip), id!(Add)) else { continue };
            let button = add.widget(cx, ids!(add_member));
            // `+` comes first, so no status, name or member change can move it under the pointer.
            button.set_action_data(HomeAction::AddMember(town));
            button.as_button().set_enabled(cx, data.workspace.writable);
            add.draw_all(cx, &mut Scope::empty());
            for seat in seats {
                let item_id = member_item_id(seat.id);
                let Some(row) = list.item(cx, item_id, id!(Seat)) else { continue };
                show_member(cx, &row, town, seat, data.selected_member == Some(seat.id),
                    item_id, &mut self.live_shown, &mut self.status_shown, self.live_call_color, self.live_share_color,
                    #[cfg(test)] &mut self.stats,
                );
                row.draw_all(cx, &mut Scope::empty());
            }
            list.items.retain_visible();
            self.live_shown.retain(|id, _| list.items.contains_key(id));
            self.status_shown.retain(|id, _| list.items.contains_key(id));
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct SeatDevices {
    #[deref] view: View,
}

impl Widget for SeatDevices {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let Some(mut list) = step.borrow_mut::<FlatList>() else { continue };
            let (Some(town), Some(member)) = (data.tree.snapshot().town, data.selected_member) else {
                list.items.retain_visible();
                continue
            };
            for row in &data.member_devices {
                let device = row.key;
                let Some(item) = list.item(cx, device_item_id(&device), id!(Device)) else { continue };
                let dot = item.status_dot(cx, ids!(device_dot));
                let kind = status_kind(row.presence);
                if dot.status() != Some(kind) { dot.set_status(cx, kind); }
                let name = item.label(cx, ids!(device_name));
                if name.text() != row.name { name.set_text(cx, &row.name); }
                let status = item.label(cx, ids!(device_status));
                if status.text() != row.detail { status.set_text(cx, &row.detail); }
                status.set_visible(cx, !row.detail.is_empty());
                let connect = item.button(cx, ids!(device_connect));
                connect.set_visible(cx, row.can_connect);
                connect.set_action_data(HomeAction::DeviceConnect { town, member, device });
                let remove = item.button(cx, ids!(device_remove));
                remove.set_visible(cx, row.can_remove);
                remove.set_action_data(HomeAction::DeviceRemove { town, member, device });
                item.draw_all(cx, &mut Scope::empty());
            }
            list.items.retain_visible();
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct TileGrid {
    #[live]
    overview: bool,
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for TileGrid {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let grid = if self.overview { &data.overview_grid } else { &data.home_grid };
            list.set_item_range(cx, 0, grid.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(entry) = grid.get(i) else { continue };
                match entry {
                    GridRow::Section(title) => {
                        let row = list.item(cx, i, id!(Section));
                        row.label(cx, ids!(section_title)).set_text(cx, title);
                        row.draw_all(cx, &mut Scope::empty());
                    }
                    GridRow::Tiles(tiles) => {
                        let row = list.item(cx, i, id!(Tiles));
                        for (slot, path) in [ids!(tile_a), ids!(tile_b), ids!(tile_c)].into_iter().enumerate() {
                            let tile = row.widget(cx, path);
                            let shown = tiles.get(slot);
                            tile.set_visible(cx, shown.is_some());
                            let Some(shown) = shown else { continue };
                            crate::lists::open_label(cx, &tile, ids!(tile_title), &shown.title);
                            let summary = tile.label(cx, ids!(tile_summary));
                            summary.set_visible(cx, !shown.summary.is_empty());
                            summary.set_text(cx, &shown.summary);
                        }
                        row.draw_all(cx, &mut Scope::empty());
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[cfg(test)]
mod member_tests {
    use super::*;
    use crate::view::Seat;
    use makepad_widgets::cx_draw::CxDraw;

    #[test]
    fn all_members_wrap_in_a_capped_scroll_area_and_keep_their_places_when_presence_changes() {
        let mut pool = test_cx::checkout_test_cx(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
        });
        let cx = &mut *pool;
        let home = cx.with_vm(|vm| {
            let value = script_eval!(vm, { use mod.widgets.* HomePage{visible: true} });
            WidgetRef::script_from_value(vm, value)
        });
        let mut data = ViewData::default();
        data.tree.set(Vec::new(), TownTree { town: Some([1; 32]), ..TownTree::default() });
        data.members.seats = (0u16..300).map(|i| {
            let mut key = [0; 32];
            key[..2].copy_from_slice(&i.to_be_bytes());
            MemberSeat {
                id: if i == 0 { MemberId::You } else { MemberId::Seat(key) },
                seat: if i == 0 { Seat::You } else { Seat::Peer },
                tone: (i != 0).then(|| crate::view::SeatTone(((i - 1) as usize % crate::view::SEAT_SLOTS) as u8)),
                presence: Presence::Here,
                live: None,
                name: if i == 0 { "You".into() } else { format!("Member {i}") },
                devices: Vec::new(),
                status: if i == 0 { String::new() } else { "here".into() },
            }
        }).collect();
        let pair = &mut data.members.seats[1];
        pair.status = "here · 2 computers".into();
        for (key, presence, status) in [(1, Presence::Here, "here"), (2, Presence::Away, "away")] {
            pair.devices.push(crate::view::SeatDevice { key: [key; 65], name: format!("Computer {key}"),
                presence, status: status.into(), is_me: false });
        }
        let pass = DrawPass::new(cx);
        let mut list = DrawList2d::new(cx);
        let header = home.widget(cx, ids!(home_members));
        let height = 600.;
        let mut draw = |cx: &mut Cx, data: &mut ViewData, width| {
            let size = dvec2(width, height);
            pass.set_size(cx, size);
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            header.draw_all(&mut frame, &mut Scope::with_data(data));
            frame.end_pass_sized_turtle();
            list.end(&mut frame);
            frame.end_pass(&pass);
            let header = header.borrow::<HomeMembers>().unwrap();
            (header.stats.a11y_evals, header.stats.dot_updates, header.stats.bg_updates, header.stats.live_updates)
        };
        assert_eq!(summary(0, 1, 2), "1 board · 2 docs", "project summaries list only nonzero counts");
        assert!(summary(0, 0, 0).is_empty(), "an empty project has no summary line");
        let list = header.flat_list(cx, ids!(list));
        let palette = crate::theme::SeatPalette::from_cx(cx);
        let badge_counts = |cx: &Cx, data: &ViewData| {
            let list = list.borrow().unwrap();
            data.members.seats.iter().map(|seat| {
                let row = &list.items[member_item_id(seat.id)].widget;
                let badge = row.widget(cx, ids!(badge));
                if let Some(tone) = seat.tone {
                    let peer = badge.widget(cx, ids!(peer));
                    assert_eq!(crate::lists::badge_colors(cx, &peer),
                        (palette.seats[tone.0 as usize], palette.ink_light), "paint for {}", seat.name);
                }
                let icons = badge.borrow::<crate::lists::SeatIcons>().unwrap();
                icons.paint_counts()
            }).collect::<Vec<_>>()
        };
        let chip_rects = |cx: &Cx, data: &ViewData| {
            let seats = &data.members.seats;
            let list = list.borrow().unwrap();
            assert_eq!(list.items.len(), seats.len() + 1, "only Add and the 300 seats are drawn");
            let add = &list.items[id!(member_tip)];
            assert_eq!(add.template, id!(Add));
            let mut rects = vec![add.widget.area().rect(cx)];
            for (i, seat) in seats.iter().enumerate() {
                let item = &list.items[member_item_id(seat.id)];
                assert_eq!(item.template, id!(Seat));
                let row = &item.widget;
                assert_eq!(row.borrow::<View>().unwrap().show_bg, data.selected_member == Some(seat.id),
                    "only the selected member has a background (member {i})");
                assert_eq!(row.label(cx, ids!(name)).text(), seat.name);
                assert_eq!(row.tip(cx, ids!(tip)).borrow().unwrap().text,
                    if data.selected_member == Some(seat.id) { "" } else { seat.status.as_str() });
                let button = row.button(cx, ids!(select));
                let mut node = a11y_tree::Node::default();
                button.borrow().unwrap().semantics(cx, &mut node);
                let label = if seat.status.is_empty() { seat.name.clone() } else { format!("{}, {}", seat.name, seat.status) };
                assert_eq!(node.name, label, "the button announces the member and their status");
                let action = button.action_data().unwrap();
                assert_eq!(action.downcast_ref::<HomeAction>(), Some(&HomeAction::Card { town: [1; 32], member: seat.id }));
                let rect = row.area().rect(cx);
                assert!(rect.size.x > 0. && rect.size.y > 0., "member {i} has a drawn chip");
                rects.push(rect);
            }
            rects
        };
        for width in [620., 350.] {
            data.members.seats[0].name = if width == 620. { "Walk A" } else { "You" }.into();
            data.selected_member = None;
            data.members.seats[5].presence = Presence::Reaching;
            data.members.seats[5].status = "reaching…".into();
            draw(cx, &mut data, width);
            let own = list.borrow().unwrap().items[member_item_id(MemberId::You)].widget.clone();
            assert_eq!(own.label(cx, ids!(badge.you.initial)).text(), if width == 620. { "W" } else { "Y" });
            let before = chip_rects(cx, &data);
            let painted = badge_counts(cx, &data);
            let add = before[0];
            assert!(add.size.x > 0. && add.pos.x.abs() <= 1., "Add starts the row at width {width}");
            let block = header.area().rect(cx);
            assert!((block.size.y - height * 0.35).abs() <= 1., "300 members fill the capped header at width {width}: {block:?}");
            assert!(before.last().unwrap().pos.y > block.pos.y + block.size.y, "the last member is below the scroll viewport");
            for pair in before.windows(2) {
                let (left, next) = (pair[0], pair[1]);
                if next.pos.x > left.pos.x {
                    assert!((next.pos.x - left.pos.x - left.size.x - 8.).abs() <= 1., "8-point gap at width {width}: {pair:?}");
                    assert!((next.center().y - left.center().y).abs() <= 1., "same line centre at width {width}: {pair:?}");
                } else {
                    assert!(next.pos.x.abs() <= 1. && next.pos.y >= left.pos.y + left.size.y, "next line at width {width}: {pair:?}");
                }
                assert!(next.pos.x + next.size.x <= width + 1., "the chip fits inside the row at width {width}");
            }
            assert_eq!(draw(cx, &mut data, width), (0, 0, 0, 0), "unchanged members need no updates at width {width}");
            assert_eq!(badge_counts(cx, &data), painted, "a steady frame performs no badge writes");
            assert_eq!(chip_rects(cx, &data), before);
            let other = &mut data.members.seats[1].devices[1];
            (other.presence, other.status) = if other.presence == Presence::Away {
                (Presence::Reaching, "reaching…".into())
            } else {
                (Presence::Away, "away".into())
            };
            assert_eq!(draw(cx, &mut data, width), (0, 0, 0, 0), "a secondary computer leaves the online seat chip unchanged");
            data.members.seats[5].presence = Presence::Away;
            data.members.seats[5].status = "away · last seen 14:02".into();
            assert_eq!(draw(cx, &mut data, width), (1, 1, 0, 0), "one status change updates one label and dot");
            assert_eq!(chip_rects(cx, &data), before, "a status change leaves every chip in place at width {width}");
            let row = list.borrow().unwrap().items[member_item_id(data.members.seats[5].id)].widget.clone();
            let dot = row.status_dot(cx, ids!(dot));
            for (presence, status, kind) in [(Presence::Working, "working", StatusKind::InProgress), (Presence::Here, "listening", StatusKind::Success)] {
                data.members.seats[5].presence = presence;
                data.members.seats[5].status = status.into();
                assert_eq!(draw(cx, &mut data, width), (1, 1, 0, 0), "the transition to {presence:?} updates one dot and its status");
                assert_eq!(dot.status(), Some(kind));
                assert_eq!(chip_rects(cx, &data), before, "the transition to {presence:?} keeps every chip in place");
            }
            let mark = row.view(cx, ids!(live_mark));
            for (live, status) in [
                (Some(LiveActivity::InCall), "here · in call"),
                (Some(LiveActivity::Sharing { session: [9; 16], title: "Fab".into() }), "here · in call · sharing Fab"),
                (None, "here"),
            ] {
                let visible = live.is_some();
                data.members.seats[5].live = live;
                data.members.seats[5].status = status.into();
                assert_eq!(draw(cx, &mut data, width), (1, 0, 0, 1), "only the live mark and accessible label change for {status}");
                assert_eq!(mark.visible(), visible);
                assert_eq!(chip_rects(cx, &data), before, "the live mark keeps every chip in place for {status}");
                assert_eq!(draw(cx, &mut data, width), (0, 0, 0, 0), "steady activity needs no writes for {status}");
                if matches!(data.members.seats[5].live, Some(LiveActivity::Sharing { .. })) {
                    let mut reapplied = home.clone();
                    cx.with_vm(|vm| {
                        let value = vm.with_reload(|vm| script_eval!(vm, { use mod.widgets.* HomePage{visible: true} }));
                        reapplied.script_apply(vm, &Apply::ScriptReapply, &mut Scope::empty(), value);
                    });
                    // The reload resets every chip's dot and mark: each dot is written once more, and the one live
                    // mark gets the share colour again.
                    assert_eq!(draw(cx, &mut data, width), (0, data.members.seats.len(), 0, 1),
                        "a style reload repaints each dot and the sharing mark in the share colour");
                    assert!(mark.visible(), "a style reload keeps a live mark visible");
                    let reloaded = badge_counts(cx, &data);
                    for (i, ((paints, _), (previous, _))) in reloaded.iter().zip(&painted).enumerate() {
                        assert_eq!(paints - previous, usize::from(i != 0), "reload repaints peer {i} once");
                    }
                    assert_eq!(chip_rects(cx, &data), before, "repainting the badges preserves every chip rect");
                    assert_eq!(draw(cx, &mut data, width), (0, 0, 0, 0), "after a style reload a steady chip needs no writes");
                    assert_eq!(badge_counts(cx, &data), reloaded, "the post-reload steady frame performs no badge writes");
                }
            }
            data.selected_member = Some(data.members.seats[5].id);
            assert_eq!(draw(cx, &mut data, width), (0, 0, 1, 0), "selecting a member updates only its background");
            // A chip with its background drawn reports the background's rect, which passes through f32: the same
            // place up to float rounding (under a thousandth of a point), never a layout move.
            let after = chip_rects(cx, &data);
            let near = |a: f64, b: f64| (a - b).abs() < 1e-3;
            assert!(after.len() == before.len() && after.iter().zip(&before).all(|(a, b)|
                near(a.pos.x, b.pos.x) && near(a.pos.y, b.pos.y) && near(a.size.x, b.size.x) && near(a.size.y, b.size.y)),
                "selecting a member leaves every chip in place at width {width}");
            data.selected_member = None;
            assert_eq!(draw(cx, &mut data, width), (0, 0, 1, 0), "deselecting needs no accessible name update");
            assert_eq!(row.tip(cx, ids!(tip)).borrow().unwrap().text, data.members.seats[5].status);
        }

        let last = list.borrow().unwrap().items[member_item_id(data.members.seats[299].id)].widget.clone();
        let block = header.area().rect(cx);
        let rect = last.area().rect(cx);
        let scroll = Event::Scroll(event::ScrollEvent {
            window_id: WindowId(1, 1), abs: block.center(),
            scroll: dvec2(0., rect.pos.y + rect.size.y - block.pos.y - block.size.y),
            modifiers: Default::default(), handled_x: Default::default(), handled_y: Default::default(),
            is_mouse: false, time: 0., phase: event::ScrollPhase::Changed,
        });
        header.handle_event(cx, &scroll, &mut Scope::with_data(&mut data));
        draw(cx, &mut data, 350.);
        assert!(block.contains(last.area().rect(cx).center()), "scrolling inside the header brings Member 299 into view");
    }
}
