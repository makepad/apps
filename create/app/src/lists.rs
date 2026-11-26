//! Visible rows for the task columns, chat, navigation and projects gallery.

use makepad_widgets::*;

/// What the lists draw.
#[derive(Default)]
pub struct ViewData {
    pub workspace: crate::view::Workspace,
    pub page: Page,
    pub compact: bool,
    pub space_ids: Vec<makepad_work_core::event::Id>,
    pub space_labels: Vec<String>,
    pub space_index: usize,
    pub share_error: Option<String>,
    pub confirm_stop: Option<makepad_work_core::event::Id>,
    pub publish: Option<crate::publish::session::LocalPublish>,
    pub open_doc: Option<makepad_work_core::event::Id>,
    pub connect: crate::link_worker::ConnectView,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Tasks,
    Board,
    Docs,
    Share,
    Publish,
    Connect,
    Projects,
    Chat,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.DocumentListBase = #(DocumentList::register_widget(vm))
    mod.widgets.ProjectGridBase = #(ProjectGrid::register_widget(vm))
    mod.widgets.NavigationListBase = #(NavigationList::register_widget(vm))
    mod.widgets.CreateChatListBase = #(CreateChatList::register_widget(vm))
    mod.widgets.CreateTaskListBase = #(CreateTaskList::register_widget(vm))
}

#[derive(Script, ScriptHook, Widget)]
pub struct CreateChatList {
    #[live]
    mention_ink: Vec4f,
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for CreateChatList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else {
                continue;
            };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let messages = &data.workspace.messages;
            list.set_item_range(cx, 0, messages.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(message) = messages.get(index) else { continue };
                let item = list.item(cx, index, if message.addressed { id!(Addressed) } else { id!(Line) });
                let author = item.widget(cx, ids!(author));
                seat_icons(cx, &author, &[message.seat]);
                item.label(cx, ids!(name)).set_text(cx, &message.name);
                item.label(cx, ids!(when)).set_text(cx, &message.time);
                let body = item.label(cx, ids!(message));
                body.set_text(cx, &message.text);
                let mention = [(0, 3, self.mention_ink)];
                body.set_inks(cx, if message.addressed { &mention } else { &[] });
                item.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct CreateTaskList {
    #[live]
    column: usize,
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for CreateTaskList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else {
                continue;
            };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let cards = data
                .workspace
                .selected()
                .map_or(&[][..], |p| p.columns.get(self.column).map_or(&[][..], Vec::as_slice));
            list.set_item_range(cx, 0, cards.len());
            while let Some(index) = list.next_visible_item(cx) {
                let Some(card) = cards.get(index) else { continue };
                let item = list.item(cx, index, id!(TaskRow));
                item.label(cx, ids!(number)).set_text(cx, &card.number);
                item.label(cx, ids!(title)).set_text(cx, &card.title);
                let authors = item.widget(cx, ids!(authors));
                seat_icons(cx, &authors, &[card.seat]);
                item.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct DocumentList {
    #[deref]
    view: View,
}

impl Widget for DocumentList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let rows = data.workspace.selected().map_or(&[][..], |p| p.docs.as_slice());
            list.set_item_range(cx, 0, rows.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(doc) = rows.get(i) else { continue };
                let row = list.item(cx, i, id!(Row));
                let button = row.button(cx, ids!(open_doc));
                button.set_text(cx, &doc.title);
                button.set_open(cx, data.open_doc == Some(doc.id));
                row.label(cx, ids!(edited_by)).set_text(cx, match doc.editor {
                    crate::view::Seat::You => "Edited by you",
                    crate::view::Seat::YourAi => "Edited by your AI",
                    crate::view::Seat::Peer => "Edited by your peer",
                    crate::view::Seat::PeerAi => "Edited by your peer’s AI",
                });
                row.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct NavigationList {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for NavigationList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else {
                continue;
            };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let projects = &data.workspace.projects;
            list.set_item_range(cx, 0, projects.len() + 2);
            while let Some(i) = list.next_visible_item(cx) {
                let row = if i == 0 {
                    let row = list.item(cx, i, id!(Header));
                    let menu = row.drop_down(cx, ids!(space_menu));
                    if !menu.has_labels(&data.space_labels) { menu.set_labels(cx, data.space_labels.clone()); }
                    menu.set_selected_item(cx, data.space_index);
                    row
                } else if let Some(project) = projects.get(i - 1) {
                    let row = list.item(cx, i, id!(Project));
                    let selected = Some(project.id) == data.workspace.current;
                    row.label(cx, ids!(name)).set_text(cx, &project.name);
                    row.label(cx, ids!(count)).set_text(cx, &project.columns[0].len().to_string());
                    highlight(cx, &row, ids!(project_mark), ids!(select_project), selected);
                    row
                } else if i == projects.len() + 1 {
                    let row = list.item(cx, i, id!(Footer));
                    row.label(cx, ids!(section))
                        .set_text(cx, &data.workspace.selected().map_or(String::new(), |p| p.name.to_uppercase()));
                    for (path, page) in [
                        (ids!(tasks_tab), Page::Tasks),
                        (ids!(board_tab), Page::Board),
                        (ids!(docs_tab), Page::Docs),
                        (ids!(share_tab), Page::Share),
                    ] {
                        let tab = row.widget(cx, path);
                        highlight(cx, &tab, ids!(marker), ids!(target), data.page == page);
                    }
                    row
                } else {
                    continue;
                };
                row.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

fn highlight(cx: &mut Cx, row: &WidgetRef, mark: &[LiveId], button: &[LiveId], selected: bool) {
    let marker = row.widget(cx, mark);
    if marker.visible() == selected {
        return;
    }
    marker.set_visible(cx, selected);
    let mut face = row.button(cx, button);
    script_apply_eval!(cx, face, {draw_bg.color: if #(selected) {mod.palette.sel} else {mod.palette.panel}});
}

fn seat_icons(cx: &mut Cx, icons: &WidgetRef, seats: &[crate::view::Seat]) {
    use crate::view::Seat::*;
    for (path, seat) in [
        (ids!(you), You),
        (ids!(your_ai), YourAi),
        (ids!(peer), Peer),
        (ids!(peer_ai), PeerAi),
    ] {
        icons.widget(cx, path).set_visible(cx, seats.contains(&seat));
    }
}

impl ViewData {
    pub fn gallery_columns(&self) -> usize {
        if self.compact {
            1
        } else {
            2
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ProjectGrid {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
}

impl Widget for ProjectGrid {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else {
                continue;
            };
            let Some(data) = scope.data.get_mut::<ViewData>() else { continue };
            let projects = &data.workspace.projects;
            let columns = data.gallery_columns();
            list.set_item_range(cx, 0, projects.len().div_ceil(columns));
            while let Some(index) = list.next_visible_item(cx) {
                let start = index * columns;
                if start >= projects.len() {
                    continue;
                }
                let row = list.item(cx, index, id!(Pair));
                let paired = columns == 2 && start + 1 < projects.len();
                row.widget(cx, ids!(second)).set_visible(cx, paired);
                row.widget(cx, ids!(spacer)).set_visible(cx, columns == 2 && !paired);
                for (p, path) in projects[start..].iter().take(columns).zip([ids!(first), ids!(second)]) {
                    let card = row.widget(cx, path);
                    card.button(cx, ids!(open_project)).set_text(cx, &p.name);
                    card.label(cx, ids!(goal))
                        .set_text(cx, if p.goal.is_empty() { "No goal yet" } else { &p.goal });
                    let [open, doing, review, done] = p.columns.each_ref().map(Vec::len);
                    card.label(cx, ids!(counts))
                        .set_text(cx, &format!("{open} open · {doing} doing · {review} review · {done} done"));
                    for (path, count) in [(ids!(open), open), (ids!(doing), doing), (ids!(review), review), (ids!(done), done)] {
                        let segment = card.view(cx, path);
                        segment.set_visible(cx, count != 0);
                        if let Some(mut segment) = segment.borrow_mut() {
                            if let Size::Fill { weight, .. } = &mut segment.walk.width {
                                *weight = count as f64;
                            }
                        };
                    }
                    let authors = card.widget(cx, ids!(authors));
                    seat_icons(cx, &authors, &p.authors);
                    authors.set_visible(cx, !p.authors.is_empty());
                }
                row.draw_all(cx, &mut Scope::empty());
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}
