//! The project workspace, task board and chat panel.
//! `@ai` at the start of a message addresses your own AI, which waits for it through the
//! `work.next` tool. See the crate's [`model`](makepad_create::model) for the rules.

use makepad_ai_services::port::{AiServicePort, PortEvent};
use makepad_ai_services::wire::{ServiceCall, ToolResult};
use makepad_create::link_worker::{LinkWorker, ToWorker};
use makepad_create::lists::{Page, ViewData};
use makepad_create::model::{wall_ms, Action, Model, Origin};
use makepad_create::tools::{self, Request};
use makepad_create::view::{DeviceStatus, LinkPrompt, LocalClock, Presence, PresenceHold, RemovedProject, Workspace};
use makepad_create::board::{canvas::{BoardCanvas, BoardCanvasAction}, gesture::{AlignTo, BoardHistory, BoardTool}, tabs::{BoardTabs, TabAction}, touched_by, BoardPicker};
use makepad_work_core::{entity::{field, kind}, event::Id};
use makepad_work_core::home::Home;
pub use makepad_widgets;
use makepad_widgets::*;
use makepad_widgets::event::TouchState;

mod call_window;
mod connect_window;
mod docs_window;
mod publish_window;

app_main!(App);

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    startup() do #(App::script_component(vm)){
        ui: Root{
            main_window := Window{
                window.title: "Makepad Work"
                window.inner_size: vec2(1120, 740)
                window.caption_bar_height_override: #(makepad_create::CAPTION_HEIGHT)
                pass +: {clear_color: palette.bg}
                caption_bar +: {
                    draw_bg +: {color: palette.chrome color_unfocused: palette.chrome}
                    caption_label +: {
                        flow: Right align: Align{y: 0.5}
                        padding: Inset{left: 12 right: 12}
                        label +: {visible: false}
                        caption_icon +: {visible: false width: 0 height: 0 margin: 0}
                        caption_window_gap := View{width: 72 height: 1}
                        Svg{
                            width: 20 height: 20 animating: false
                            draw_svg +: {svg: crate_resource("self://resources/icon.svg")}
                        }
                        caption_title := Label{
                            width: Fill padding: 0 margin: Inset{left: 10}
                            max_lines: 1 text_overflow: Ellipsis
                            text: "Makepad Work"
                            draw_text +: {color: palette.muted text_style: theme.font_code{font_size: 9}}
                        }
                        connections := CaptionConnections{}
                    }
                }
                body +: {
                    flow: Down spacing: 0 padding: 0
                    compact_tabs := CompactNavigation{}
                    compact_rule := PanelLine{visible: false}
                    View{
                        width: Fill height: Fill flow: Right spacing: 0
                        left_rail := SolidView{
                            width: 169 height: Fill flow: Down
                            padding: Inset{left: 8 right: 8 top: 10 bottom: 10}
                            draw_bg.color: palette.panel
                            navigation := SideNavigation{}
                        }
                        left_rule := PanelLine{width: 1 height: Fill}
                        main_area := View{
                            width: Fill height: Fill flow: Down
                            talk_page := TalkPage{visible: false}
                            board := TaskBoard{}
                            canvas := WhiteboardPage{visible: false}
                            docs_page := DocumentsPage{visible: false}
                            publish_page := PublishPage{visible: false}
                            hosted := HostedAppsPage{visible: false}
                            projects_view := ProjectGallery{visible: false}
                        }
                        right_rule := PanelLine{width: 1 height: Fill}
                        chat_panel := ChatRail{}
                    }
                    layers := OverlayLayers{
                        tip_layer +: {draw_bg.color: palette.sel draw_text.color: palette.text}
                    }
                }
            }
        }
    }
}

/// A `work.next` call held until something arrives for the AI.
struct Wait {
    call_id: String,
}

#[derive(Debug)]
struct DismissRemovedProject(Id);

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    model: Option<Model>,
    #[rust]
    board_history: std::collections::HashMap<Id, BoardHistory>,
    #[rust]
    board_project: Option<Id>,
    #[rust]
    board_picker: BoardPicker,
    #[rust]
    receiving_peer: bool,
    /// Why the model isn't there (the home couldn't be opened), shown in the chat.
    #[rust]
    problem: Option<String>,
    #[rust]
    port: Option<AiServicePort>,
    #[rust]
    wait: Option<Wait>,
    #[rust]
    view: ViewData,
    #[rust]
    clock: LocalClock,
    #[rust]
    document: Option<makepad_create::docs::OpenDocument>,
    #[rust]
    doc_timer: Timer,
    #[rust]
    publish_timer: Timer,
    #[rust]
    presence_timer: Timer,
    #[rust]
    presence: PresenceHold,
    #[rust]
    clock_load: Option<makepad_platform::thread::TaskHandle<LocalClock>>,
    /// The first contact or linked peer, independent of the caption panel.
    #[rust]
    link: Option<LinkWorker>,
    /// Commands the worker's queue had no room for yet, in order.
    #[rust]
    link_queue: std::collections::VecDeque<ToWorker>,
    /// A room-changing message waits here if a dirty document could not yet be saved.
    #[rust]
    unread_link: Option<Vec<u8>>,
    #[rust]
    draft_room: Option<makepad_work_core::event::Id>,
    #[rust]
    room_drafts: std::collections::BTreeMap<makepad_work_core::event::Id, (String, String)>,
    /// The person turned off "Link directly on the same network" (on by default).
    #[rust]
    link_lan_off: bool,
    #[rust]
    show_another_ai: bool,
}

fn reply(port: &Option<AiServicePort>, result: ToolResult) {
    if let Some(port) = port {
        port.reply(result);
    }
}

/// `@ai` at the start of a message: the message is for my own AI (the field, not the text).
fn split_at_ai(text: &str) -> (bool, String) {
    let trimmed = text.trim_start();
    if trimmed.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("@ai")) && trimmed[3..].chars().next().is_none_or(char::is_whitespace) {
        (true, trimmed[3..].trim().to_string())
    } else {
        (false, text.trim().to_string())
    }
}

fn compact_tabs() -> [(&'static [LiveId], Page); 6] {
    [
        (ids!(tasks_icon.button), Page::Tasks),
        (ids!(board_icon.button), Page::Board),
        (ids!(docs_icon.button), Page::Docs),
        (ids!(share_icon.button), Page::Share),
        (ids!(projects_icon.button), Page::Projects),
        (ids!(chat_icon.button), Page::Chat),
    ]
}

impl App {
    fn open_model(&mut self) {
        let Some(dir) = Home::default_dir() else {
            self.problem = Some("no home directory: set MAKEPAD_CREATE_HOME".into());
            return;
        };
        match Model::open(&dir) {
            Ok((model, problems)) => {
                for problem in problems {
                    log!("work: {problem}");
                }
                if let Err(e) = makepad_remote::discovery::publish(&model.home.endpoint_path()) {
                    log!("work: the endpoint file was not written ({e}); an agent reaches this window by its pid");
                }
                self.model = Some(model);
            }
            Err(e) => {
                self.problem = Some(format!(
                    "{e}. Close the other one, or start this one with MAKEPAD_CREATE_HOME set to another directory."
                ));
            }
        }
    }

    /// Rebuilds what the window shows from the model.
    fn read_workspace(&mut self) {
        if let Some(model) = &self.model {
            let (ids, labels): (Vec<_>, Vec<_>) = model.spaces().into_iter().unzip();
            self.view.space_index = ids.iter().position(|id| *id == model.open_id()).unwrap_or(0);
            self.view.space_ids = ids;
            self.view.space_labels = labels;
            if self.view.confirm_stop != Some(model.open_id()) { self.view.confirm_stop = None; }
        }
        let removed = self.view.workspace.removed.take();
        let draft = self.view.workspace.project_name_draft.take();
        self.view.workspace = self
            .model
            .as_ref()
            .map(|m| Workspace::read(m, self.wait.is_some(), &self.clock))
            .unwrap_or_default();
        self.view.workspace.removed = removed.filter(|r| self.view.workspace.writable && self.model.as_ref()
            .and_then(|m| m.state.entities.get(&r.project))
            .is_some_and(|p| p.flag(field::ARCHIVED) && p.fields.get(&field::ARCHIVED).is_some_and(|f| f.writer == r.event)));
        if self.view.workspace.writable { self.view.workspace.project_name_draft = draft; }
    }

    fn select_project(&mut self, id: Id) {
        if let Some(model) = &mut self.model {
            model.select_project(id);
            let current = model.current_project().map(|e| e.id);
            self.view.workspace.current = current;
            self.view.workspace.removable = current.filter(|id| model.can_remove_project(*id));
        }
    }

    fn undo_removed_project(&mut self, cx: &mut Cx) {
        let Some((of, project)) = self.view.workspace.removed.as_ref().map(|r| (r.event, r.project)) else { return };
        if self.act(cx, Action::Undo { of }).is_some() {
            self.select_project(project);
            self.update_page(cx);
        }
    }

    fn refresh(&mut self, cx: &mut Cx) {
        if let Some(room) = self.model.as_ref().map(Model::open_id) {
            if self.draft_room != Some(room) {
                let task = self.ui.text_input(cx, ids!(task_input));
                if let Some(old) = self.draft_room.replace(room) {
                    self.view.workspace.project_name_draft = None;
                    let form = self.view.show_conversation(cx, &self.ui);
                    let chat = form.text_input(cx, ids!(chat_input));
                    let drafts = (chat.text(), task.text());
                    if !drafts.0.is_empty() || !drafts.1.is_empty() { self.room_drafts.insert(old, drafts); }
                    let (conversation, title) = self.room_drafts.remove(&room).unwrap_or_default();
                    chat.set_text(cx, &conversation);
                    task.set_text(cx, &title);
                    self.view.open_doc = None;
                }
            }
        }
        if self.receiving_peer {
            self.select_board(cx);
            if let Some(model) = &self.model {
                let surface = self.ui.widget(cx, ids!(canvas.surface));
                if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() {
                    canvas.peer_snapshot(cx, &model.state, self.view.page == Page::Board);
                };
            }
        }
        self.read_workspace();
        self.update_page(cx);
    }

    fn update_space_notice(&self, cx: &mut Cx) {
        let problem = self.model.as_ref().and_then(Model::space_problem);
        let notice = self.model.as_ref().and_then(Model::space_notice);
        self.ui.widget(cx, ids!(space_notice)).set_visible(cx, problem.is_some() || notice.is_some());
        self.ui.widget(cx, ids!(space_notice_ok)).set_visible(cx, problem.is_none());
        if let Some(problem) = problem {
            self.ui.label(cx, ids!(space_notice_text)).set_text(cx, problem);
        } else if let Some(path) = notice {
            self.ui.label(cx, ids!(space_notice_text)).set_text(cx, &format!(
                "Work started a new space. Your earlier one is kept, unchanged, in {}.", path.display()));
        }
    }

    fn update_presence(&mut self, cx: &mut Cx) {
        let wall = wall_ms();
        let now = (cx.seconds_since_app_start() * 1000.0) as u64;
        let connect = &self.view.connect;
        let linked = connect.linked.as_ref().map(|peer| match (&connect.peer, &self.model) {
            (Some(device), Some(model)) => model.device_label(device),
            _ => peer.peer_name.as_str(),
        });
        let reaching = connect.reaching().and_then(|(device, refusal)| {
            Some((self.model.as_ref()?.device_label(device), connect.reaching_since?, refusal.is_some()))
        });
        let next = Presence::read(connect, linked, reaching, self.model.as_ref().and_then(Model::last_linked_device),
            &self.clock, wall / 1000, self.view.workspace.listening, self.words_checked());
        let date_delay = next.peer.refresh_after_secs.map(|secs| secs * 1000 - wall % 1000);
        let shown = self.presence.update(next, now);
        let peer = &shown.peer;
        let compact = self.view.compact;
        let offline = if compact { "offline" } else { "not connected" };
        let detail = if compact && peer.status == DeviceStatus::NotConnected { offline.into() } else { peer.status.text() };
        for (path, name, status) in [
            (ids!(caption_you), "You", "online"),
            (ids!(caption_ai), "Your AI", if shown.listening { "listening" } else { "not connected" }),
            (ids!(caption_peer), peer.name.as_str(), detail.as_str()),
            (ids!(caption_peer_ai), peer.ai_name.as_str(), offline),
        ] {
            let seat = self.ui.widget(cx, path);
            seat.label(cx, ids!(name)).set_text(cx, name);
            seat.label(cx, ids!(status)).set_text(cx, status);
            cx.with_vm(|vm| {
                if let Some(a11y) = vm.value(seat.script_source(), id!(a11y)).as_object() {
                    let label = vm.new_string(&format!("{name} · {status}"));
                    vm.set_value(a11y, id!(label), label);
                }
            });
        }
        for (path, visible) in [
            (ids!(peer_online), peer.status == DeviceStatus::Connected),
            (ids!(peer_offline), matches!(peer.status, DeviceStatus::Offline(_) | DeviceStatus::NotConnected)),
            (ids!(peer_reaching), matches!(peer.status, DeviceStatus::Reaching(_))),
            (ids!(ai_online), shown.listening),
            (ids!(ai_offline), !shown.listening),
        ] {
            self.ui.widget(cx, path).set_visible(cx, visible);
        }
        if let Some(mut panel) = self.ui.popover(cx, ids!(connections)).borrow_mut() {
            let seats = if compact { 425. } else { 510. };
            let prompt = match shown.prompt { LinkPrompt::None => 0., LinkPrompt::Linking => 62., LinkPrompt::CheckWords => 124. };
            panel.walk.width = Size::Fixed(seats + prompt);
        }
        let prompt = self.ui.label(cx, ids!(caption_link));
        prompt.set_visible(cx, shown.prompt != LinkPrompt::None);
        prompt.set_text(cx, match shown.prompt {
            LinkPrompt::None => "",
            LinkPrompt::Linking => "Linking…",
            LinkPrompt::CheckWords => "Check device words",
        });
        cx.stop_timer(std::mem::take(&mut self.presence_timer));
        let hold_delay = self.presence.deadline().map(|t| t.saturating_sub(now));
        if let Some(ms) = hold_delay.into_iter().chain(date_delay).min() {
            self.presence_timer = cx.start_timeout(ms as f64 / 1000.0);
        }
    }

    fn fit_window(&mut self, cx: &mut Cx, width: f64, reapply: bool) {
        let compact = width <= 980.0;
        if width <= 0.0 || (!reapply && self.view.compact == compact) {
            return;
        }
        self.view.compact = compact;
        for (path, wide, narrow) in [
            (ids!(caption_you.words), 42., 40.), (ids!(caption_ai.words), 82., 82.),
            (ids!(caption_peer.words), 118., 104.), (ids!(caption_peer_ai.words), 82., 62.),
        ] {
            if let Some(mut words) = self.ui.view(cx, path).borrow_mut() {
                words.walk.width = Size::Fixed(if compact { narrow } else { wide });
            }
        }
        for path in [ids!(caption_you.badge), ids!(caption_peer_ai.badge)] {
            self.ui.widget(cx, path).set_visible(cx, !compact);
        }
        for path in [ids!(left_rail), ids!(left_rule)] {
            self.ui.widget(cx, path).set_visible(cx, !compact);
        }
        self.ui.widget(cx, ids!(compact_tabs)).set_visible(cx, compact);
        self.ui.widget(cx, ids!(compact_rule)).set_visible(cx, compact);
        if let Some(mut columns) = self.ui.view(cx, ids!(columns)).borrow_mut() {
            columns.layout.flow = if compact { Flow::Down } else { Flow::right() };
        }
        self.update_page(cx);
    }

    fn update_page(&mut self, cx: &mut Cx) {
        let menu = self.ui.drop_down(cx, ids!(compact_space));
        if !menu.has_labels(&self.view.space_labels) { menu.set_labels(cx, self.view.space_labels.clone()); }
        menu.set_selected_item(cx, self.view.space_index);
        let line = self.model.as_ref().and_then(Model::read_only_line);
        let conversation = self.view.show_conversation(cx, &self.ui);
        let read_only = conversation.label(cx, ids!(room_read_only));
        read_only.set_visible(cx, line.is_some());
        if let Some(line) = &line { read_only.set_text(cx, line); }
        let input = conversation.text_input(cx, ids!(chat_input));
        if input.is_read_only() != line.is_some() { input.set_is_read_only(cx, line.is_some()); }
        self.update_presence(cx);
        self.update_space_notice(cx);
        self.update_connect(cx);
        self.show_docs(cx);
        self.show_publish(cx);
        let board_shown = self.view.page == Page::Board;
        if board_shown && self.board_project != self.view.workspace.current { self.select_board(cx); }
        if let Some(mut tabs) = self.ui.widget(cx, ids!(board_tabs)).borrow_mut::<BoardTabs>() {
            tabs.edit_context(cx, board_shown && self.view.workspace.writable);
        }
        let surface = self.ui.widget(cx, ids!(canvas.surface));
        if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() {
            canvas.edit_context(cx, board_shown, self.view.workspace.writable);
            if board_shown {
                self.ui.widget(cx, ids!(board_empty)).set_visible(cx, canvas.show_invitation());
                self.align_toolbar(cx, canvas.can_align());
            }
        }
        let chat = self.view.page == Page::Chat;
        if self.view.compact {
            for (path, page) in compact_tabs() {
                self.ui.button(cx, path).set_open(cx, page == self.view.page);
            }
        } else {
            let header = self.ui.portal_list(cx, ids!(navigation.list)).item(cx, 0, id!(Header));
            header.widget(cx, ids!(talk_tab.marker)).set_visible(cx, chat);
            header.button(cx, ids!(talk_tab.target)).set_open(cx, chat);
        }
        for (page, path) in [
            (Page::Tasks, ids!(board)),
            (Page::Board, ids!(canvas)),
            (Page::Docs, ids!(docs_page)),
            (Page::Share, ids!(hosted)),
            (Page::Publish, ids!(publish_page)),
            (Page::Projects, ids!(projects_view)),
        ] {
            self.ui.widget(cx, path).set_visible(cx, self.view.page == page);
        }
        let data = &self.view.workspace;
        let project = data.selected().map_or("Workspace", |p| p.name.as_str());
        let empty = conversation.label(cx, ids!(chat_empty));
        empty.set_visible(cx, data.messages.is_empty());
        empty.set_text(cx, self.problem.as_deref().unwrap_or("Start the conversation here."));
        for (i, path) in [ids!(open), ids!(doing), ids!(review), ids!(done)].into_iter().enumerate() {
            let column = self.ui.widget(cx, ids!(columns)).widget(cx, path);
            let count = data.selected().map_or(0, |p| p.columns[i].len());
            column.label(cx, ids!(count)).set_text(cx, &count.to_string());
            column.widget(cx, ids!(empty)).set_visible(cx, count == 0);
            column.widget(cx, ids!(cards)).set_visible(cx, count != 0);
        }
        let goal = data.selected().map_or("", |p| p.goal.as_str());
        self.ui
            .label(cx, ids!(project_goal))
            .set_text(cx, if goal.is_empty() { "No goal yet" } else { goal });
        let writable = data.writable && data.current.is_some();
        self.ui.widget(cx, ids!(task_input)).set_visible(cx, writable);
        self.ui.button(cx, ids!(capture_task)).set_enabled(cx, writable);
        self.ui.button(cx, ids!(add_focus)).set_enabled(cx, writable);
        self.ui
            .label(cx, ids!(caption_title))
            .set_text(cx, &format!("Makepad Work · {project}"));
        self.ui.label(cx, ids!(project_name)).set_text(cx, project);
        self.ui.redraw(cx);
    }

    /// Answers a held wait when something is pending for the AI.
    fn deliver(&mut self) {
        let Some(model) = self.model.as_mut() else { return };
        if self.wait.is_none() {
            return;
        }
        if let Ok(Some(answer)) = model.next(None) {
            let wait = self.wait.take().unwrap();
            reply(&self.port, ToolResult::ok(&wait.call_id, answer, "your person asked you"));
        }
    }

    fn call(&mut self, cx: &mut Cx, call: ServiceCall) {
        let Some(model) = self.model.as_mut() else {
            let why = self.problem.clone().unwrap_or_else(|| "Create is not ready".into());
            reply(&self.port, ToolResult::unavailable(&call.call_id, why));
            return;
        };
        let request = match tools::read(&call.tool, &call.args) {
            Ok(r) => r,
            Err(e) => {
                reply(&self.port, ToolResult::refused(&call.call_id, e));
                return;
            }
        };
        let changes_view = matches!(request, Request::Next { .. } | Request::Do(_));
        let result = match request {
            Request::Next { after } => {
                if self.wait.is_some() {
                    ToolResult::refused(
                        &call.call_id,
                        "another agent is already waiting for this seat's items; one wait at a time",
                    )
                } else {
                    match model.next(after) {
                        Ok(Some(answer)) => ToolResult::ok(&call.call_id, answer, "your person asked you"),
                        Ok(None) => {
                            self.show_another_ai = false;
                            self.wait = Some(Wait { call_id: call.call_id });
                            self.refresh(cx);
                            return;
                        }
                        Err(e) => ToolResult::refused(&call.call_id, e),
                    }
                }
            }
            Request::ChatRead { limit } => ToolResult::ok(&call.call_id, model.chat_read_json(limit), "the chat you may read"),
            Request::DocRead { doc, after } => match (|| {
                if self.document.as_ref().is_some_and(|d| d.id == doc && d.dirty()) {
                    self.flush_doc(cx)?;
                    self.update_page(cx);
                }
                self.model.as_mut().unwrap().doc_read_json(doc, after)
            })() {
                Ok(answer) => ToolResult::ok(&call.call_id, answer, "the shared document"),
                Err(e) => ToolResult::refused(&call.call_id, e),
            },
            Request::PublishMedia => match self.view.publish.as_ref().ok_or_else(|| "publish is unavailable".to_string()).and_then(|p| p.catalog()) {
                Ok(list) => ToolResult::ok(&call.call_id, list, "local media"),
                Err(e) => ToolResult::refused(&call.call_id, e),
            },
            Request::Publish { ids, title, text } => match self.view.publish.as_mut().ok_or_else(|| "publish is unavailable".to_string())
                .and_then(|p| p.review(ids, title, text)) {
                Ok(()) => {
                    self.view.page = Page::Publish;
                    self.update_page(cx);
                    ToolResult::ok(&call.call_id, "{}", "draft ready for local review")
                }
                Err(e) => ToolResult::refused(&call.call_id, e),
            },
            Request::DevicesRetry => match self.retry_devices(cx) {
                Ok(()) => ToolResult::ok(&call.call_id, "Linking is on.", "known devices loaded"),
                Err(e) => ToolResult::failed(&call.call_id, format!("Linking is off. {e}")),
            },
            Request::DevicesForget => match self.review_forget_devices(cx) {
                Ok(()) => ToolResult::ok(&call.call_id, "Open Connections in the caption to confirm.", "waiting for local confirmation"),
                Err(e) => ToolResult::refused(&call.call_id, e),
            },
            Request::TasksList => ToolResult::ok(&call.call_id, model.tasks_json(), "the tasks"),
            Request::BoardList => ToolResult::ok(&call.call_id, model.boards_json(), "the project's boards"),
            Request::BoardRead { board, rect, after } => match model.board_read_json(board, rect, after) {
                Ok(answer) => ToolResult::ok(&call.call_id, answer, "the shared board"),
                Err(e) => ToolResult::refused(&call.call_id, e),
            },
            Request::Do(action) => {
                let tool = tools::tool_of(&action);
                match self.dispatch(action, Origin::Tool) {
                    Ok(id) => {
                        self.board_event(cx, &id);
                        ToolResult::ok(&call.call_id, tools::action_reply(self.model.as_ref().unwrap(), &id, tool), tool)
                    }
                    Err(e) => ToolResult::refused(&call.call_id, e),
                }
            }
        };
        reply(&self.port, result);
        if changes_view {
            self.refresh(cx);
        }
    }

    fn drain_port(&mut self, cx: &mut Cx, event: &Event) {
        let events = match self.port.as_mut() {
            Some(port) => port.handle_event(cx, event),
            None => return,
        };
        for ev in events {
            match ev {
                PortEvent::Call(call) => self.call(cx, call),
                PortEvent::Cancel { call_id } => {
                    if self.wait.as_ref().is_some_and(|w| w.call_id == call_id) {
                        self.wait = None;
                        self.refresh(cx);
                    }
                }
                PortEvent::Registered(_) | PortEvent::ChatOpen { .. } | PortEvent::Subscribe { .. } | PortEvent::Unsubscribe { .. } => {}
            }
        }
    }

    fn act(&mut self, cx: &mut Cx, action: Action) -> Option<Id> {
        let event = match self.dispatch(action, Origin::Ui) {
            Ok(id) => { self.board_event(cx, &id); Some(id) }
            Err(e) => { log!("work: {e}"); None }
        };
        self.deliver();
        self.refresh(cx);
        event
    }

    fn select_board(&mut self, cx: &mut Cx) {
        let Some(model) = &self.model else { return };
        self.board_project = self.view.workspace.current;
        let index = self.board_picker.refresh(&model.state, self.board_project);
        let id = index.map(|i| self.board_picker.boards[i]);
        if let Some(mut tabs) = self.ui.widget(cx, ids!(board_tabs)).borrow_mut::<BoardTabs>() {
            tabs.show(cx, &self.board_picker, index);
        }
        let surface = self.ui.widget(cx, ids!(canvas.surface));
        if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() {
            let tool = canvas.tool;
            canvas.select_board(cx, &model.state, id, self.view.page == Page::Board);
            if tool != canvas.tool { self.show_board_tool(cx, &canvas); }
        };
    }

    fn board_event(&mut self, cx: &mut Cx, id: &Id) {
        let Some(model) = &self.model else { return };
        let Some(event) = model.event(id) else { return };
        let ids = touched_by(event, |id| model.event(id));
        if ids.iter().any(|id| model.state.entities.get(id).is_some_and(|e| e.kind == kind::BOARD && e.project == self.board_project)) {
            self.select_board(cx);
        }
        let surface = self.ui.widget(cx, ids!(canvas.surface));
        if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() {
            canvas.applied(cx, &self.model.as_ref().unwrap().state, &ids, self.view.page == Page::Board);
        };
    }

    fn align_toolbar(&self, cx: &mut Cx, enabled: bool) {
        for (path, visible) in [(ids!(align_menu), enabled), (ids!(align_disabled), !enabled)] {
            let widget = self.ui.widget(cx, path);
            if widget.visible() != visible { widget.set_visible(cx, visible); }
        }
    }

    fn board_tools() -> [(BoardTool, &'static [LiveId]); 5] {
        [(BoardTool::Select, ids!(tool_select)), (BoardTool::Sticky, ids!(tool_sticky)),
         (BoardTool::Text, ids!(tool_text)), (BoardTool::Shape, ids!(tool_shape)), (BoardTool::Section, ids!(tool_section))]
    }

    fn show_board_tool(&self, cx: &mut Cx, canvas: &BoardCanvas) {
        self.ui.widget(cx, ids!(board_empty)).set_visible(cx, canvas.show_invitation());
        for (tool, path) in Self::board_tools() {
            let mut face = self.ui.button(cx, path);
            let active = canvas.tool == tool;
            script_apply_eval!(cx, face, {draw_bg.color: if #(active) {mod.palette.sel} else {mod.palette.panel}});
        }
    }

    fn board_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let surface = self.ui.widget(cx, ids!(canvas.surface));
        // A switch may already have hidden the board that owned the field.
        for a in actions.iter().filter_map(|a| a.as_widget_action()).filter(|w| w.widget_uid == surface.widget_uid()) {
            if let Some(BoardCanvasAction::CommitText { board, action }) = a.action.downcast_ref::<BoardCanvasAction>() {
                if self.view.workspace.writable && self.model.as_ref().is_some_and(|m| m.state.entities.contains_key(board)) {
                    if let Some(id) = self.act(cx, action.clone()) {
                        self.board_history.entry(*board).or_default().record(id);
                    }
                }
            }
        }
        if self.view.page != Page::Board { return }
        let Some(project) = self.board_project else { return };
        let tabs = self.ui.widget(cx, ids!(board_tabs)).borrow_mut::<BoardTabs>()
            .map(|mut tabs| tabs.actions(cx, actions)).unwrap_or_default();
        for command in tabs {
            let board = match command {
                TabAction::New => {
                    let title = self.board_picker.next_title();
                    let Some(id) = self.act(cx, Action::AddBoard { project, title }) else { continue };
                    self.board_history.entry(id).or_default().record(id);
                    id
                }
                TabAction::Choose(id) => id,
                TabAction::Rename(board, title) => {
                    if let Some(action) = self.board_picker.rename(board, &title) {
                        if let Some(id) = self.act(cx, action) {
                            self.board_history.entry(board).or_default().record(id);
                        }
                    }
                    continue
                }
                TabAction::FocusCanvas => {
                    cx.set_key_focus_from_user(surface.area());
                    continue
                }
            };
            self.board_picker.choose(project, board);
            self.select_board(cx);
            self.update_page(cx);
            cx.set_key_focus_from_user(surface.area());
        }
        let board = surface.borrow::<BoardCanvas>().and_then(|b| b.board_id());
        for (tool, path) in Self::board_tools() {
            let button = self.ui.button(cx, path);
            if tool != BoardTool::Select && button.pressed(actions) && cx.fingers.is_area_captured(button.area()) {
                let toolbar = self.ui.widget(cx, ids!(board_toolbar));
                if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() { canvas.drag_tool(cx, tool, toolbar); }
            }
            if button.clicked(actions) {
                if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() { canvas.arm(cx, tool); }
            }
        }
        let menu = self.ui.drop_down(cx, ids!(align_menu));
        if let Some(choice) = menu.selected(actions).and_then(|i| AlignTo::ALL.get(i).copied()) {
            cx.set_key_focus_from_user(surface.area());
            let edit = surface.borrow::<BoardCanvas>().and_then(|b| b.alignment(choice));
            if let (Some(board), Some(id)) = (board, edit.and_then(|action| self.act(cx, action))) {
                self.board_picker.choose(project, board);
                self.board_history.entry(board).or_default().record(id);
            }
        }
        for a in actions.iter().filter_map(|a| a.as_widget_action())
            .filter(|w| w.widget_uid == surface.widget_uid())
        {
            let Some(action) = a.action.downcast_ref::<BoardCanvasAction>() else { continue };
            match action {
                BoardCanvasAction::CommitText { .. } => {}
                BoardCanvasAction::ToolChanged => {
                    if let Some(canvas) = surface.borrow::<BoardCanvas>() { self.show_board_tool(cx, &canvas); }
                }
                BoardCanvasAction::SelectionChanged => {
                    if let Some(b) = surface.borrow::<BoardCanvas>() { self.align_toolbar(cx, b.can_align()); }
                }
                BoardCanvasAction::Place(item) => {
                    if let Some((board, events)) = item.place(project, board, |action| self.act(cx, action)) {
                        if let Some(&id) = events.last().filter(|id| **id != board) {
                            if let Some(mut canvas) = surface.borrow_mut::<BoardCanvas>() { canvas.placed(cx, id); }
                        }
                        self.board_picker.choose(project, board);
                        self.board_history.entry(board).or_default().record_step(events);
                    }
                }
                BoardCanvasAction::Act(action) => {
                    let Some(board) = board else { continue };
                    if let Some(id) = self.act(cx, action.clone()) {
                        self.board_picker.choose(project, board);
                        self.board_history.entry(board).or_default().record(id);
                    }
                }
                BoardCanvasAction::Undo | BoardCanvasAction::Redo => {
                    let redo = matches!(action, BoardCanvasAction::Redo);
                    let Some(board) = self.board_picker.history_board(project, board, redo) else { continue };
                    if let Some(mut history) = self.board_history.remove(&board) {
                        history.step(redo, |action| self.act(cx, action));
                        self.board_history.insert(board, history);
                        if !redo {
                            self.board_picker.after_undo(project, board);
                        } else if self.board_picker.boards.contains(&board) {
                            self.board_picker.choose(project, board);
                            if surface.borrow::<BoardCanvas>().and_then(|c| c.board_id()) != Some(board) {
                                self.select_board(cx);
                                self.update_page(cx);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        self.clock_load = cx
            .task_pool()
            .submit(makepad_platform::thread::Class::Background, LocalClock::load)
            .ok();
        self.ui.drop_down(cx, ids!(align_menu)).set_face_labels(cx, vec!["Align".into(); 6]);
        self.open_model();
        // A known device is reached again with no words, for as long as it is wanted.
        self.reconnect_start(cx);
        let width = self.ui.window(cx, ids!(main_window)).get_inner_size(cx).x;
        self.fit_window(cx, width, true);
        self.open_publish(cx);
        self.port = AiServicePort::open(cx, tools::manifest(self.view.publish.as_ref().is_some_and(|p| p.enabled())));
        self.refresh(cx);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(space_notice_ok)).clicked(actions) {
            if let Some(model) = &mut self.model {
                if let Err(e) = model.dismiss_space_notice() { log!("work: keeping the space notice: {e}"); }
            }
            self.update_space_notice(cx);
        }
        self.board_actions(cx, actions);
        self.doc_actions(cx, actions);
        self.publish_actions(cx, actions);
        self.connect_actions(cx, actions);
        if self.view.compact {
            for (path, page) in compact_tabs() {
                if self.ui.button(cx, path).clicked(actions) {
                    self.view.page = page;
                    self.update_page(cx);
                }
            }
        }
        if let Some(index) = self.ui.drop_down(cx, ids!(compact_space)).changed(actions) {
            if let Some(&room) = self.view.space_ids.get(index) { self.switch_space(cx, room); }
        }
        let navigation = self.ui.portal_list(cx, ids!(navigation.list));
        for (index, row) in navigation.items_with_actions(actions) {
            if index == 0 {
                let input = row.text_input(cx, ids!(new_project_name));
                if row.button(cx, ids!(new_project)).clicked(actions) && self.view.workspace.writable {
                    let draft = self.view.workspace.project_name_draft.get_or_insert_with(String::new);
                    input.set_text(cx, draft);
                    if input.visible() { input.set_key_focus(cx); }
                    self.ui.redraw(cx);
                    break
                }
                if self.view.workspace.project_name_draft.is_some() {
                    if input.escaped(actions) {
                        self.view.workspace.project_name_draft = None;
                        input.set_visible(cx, false);
                        row.button(cx, ids!(new_project)).set_key_focus(cx);
                        self.ui.redraw(cx);
                        break
                    }
                    if let Some((title, _)) = input.returned(actions) {
                        if self.act(cx, Action::AddProject { title }).is_some() {
                            self.view.workspace.project_name_draft = None;
                            self.view.page = Page::Tasks;
                            self.update_page(cx);
                        }
                        break
                    }
                    if let Some(text) = input.changed(actions) { self.view.workspace.project_name_draft = Some(text); }
                }
            }
            if let Some(choice) = row.drop_down(cx, ids!(space_menu)).changed(actions) {
                if let Some(&room) = self.view.space_ids.get(choice) { self.switch_space(cx, room); }
                continue
            }
            if row.button(cx, ids!(undo_project)).clicked(actions) {
                self.undo_removed_project(cx);
                break
            }
            let slot = index.checked_sub(1);
            if row.button(cx, ids!(remove_project)).clicked(actions) {
                if let Some((i, project, title)) = slot.and_then(|i| self.view.workspace.projects.get(i)
                    .filter(|p| Some(p.id) == self.view.workspace.removable).map(|p| (i, p.id, p.name.clone()))) {
                    if let Some(event) = self.act(cx, Action::RemoveProject { project }) {
                        self.view.workspace.removed = Some(RemovedProject { project, event, index: i, title });
                        self.ui.redraw(cx);
                    }
                }
                break
            }
            let page = if row.button(cx, ids!(projects)).clicked(actions) {
                Some(Page::Projects)
            } else if row.button(cx, ids!(select_project)).clicked(actions) {
                if let Some(id) = slot.and_then(|i| self.view.workspace.projects.get(i)).map(|p| p.id) {
                    self.select_project(id);
                }
                Some(Page::Tasks)
            } else {
                [
                    (ids!(talk_tab.target), Page::Chat),
                    (ids!(tasks_tab.target), Page::Tasks),
                    (ids!(board_tab.target), Page::Board),
                    (ids!(docs_tab.target), Page::Docs),
                    (ids!(share_tab.target), Page::Share),
                ]
                .into_iter()
                .find_map(|(path, page)| row.button(cx, path).clicked(actions).then_some(page))
            };
            if let Some(page) = page {
                self.view.page = page;
                self.update_page(cx);
            }
        }
        let gallery = self.ui.portal_list(cx, ids!(gallery.list));
        for (index, row) in gallery.items_with_actions(actions) {
            for (column, path) in [ids!(first.open_project), ids!(second.open_project)].into_iter().enumerate() {
                if row.button(cx, path).clicked(actions) {
                    let i = index * self.view.gallery_columns() + column;
                    if let Some(p) = self.view.workspace.projects.get(i) {
                        self.select_project(p.id);
                        self.view.page = Page::Tasks;
                        self.update_page(cx);
                    }
                }
            }
        }
        let conversation = self.view.conversation(cx, &self.ui);
        let chat_input = conversation.text_input(cx, ids!(chat_input));
        if let Some((draft, _)) = chat_input.returned(actions) {
            let (to_ai, text) = split_at_ai(&draft);
            if conversation.visible() && !chat_input.is_read_only() {
                if !text.is_empty() && self.act(cx, Action::PostChat { text, to_ai, reply_to: None }).is_some() {
                    chat_input.set_text(cx, "");
                }
                chat_input.set_key_focus(cx);
            }
        }
        let task_input = self.ui.text_input(cx, ids!(task_input));
        if self.ui.button(cx, ids!(add_focus)).clicked(actions) || self.ui.button(cx, ids!(capture_task)).clicked(actions) {
            task_input.set_key_focus(cx);
        }
        if task_input.returned(actions).is_some() && self.view.workspace.writable && self.view.workspace.current.is_some() {
            let title = task_input.text();
            if !title.trim().is_empty() {
                task_input.set_text(cx, "");
                self.act(
                    cx,
                    Action::AddTask {
                        title,
                        text: String::new(),
                    },
                );
                task_input.set_key_focus(cx);
            }
        }
        if self.view.workspace.removed.as_ref().is_some_and(|r| actions.iter().any(|a|
            a.downcast_ref::<DismissRemovedProject>().is_some_and(|dismiss| dismiss.0 == r.event))) {
            self.view.workspace.removed = None;
            self.ui.redraw(cx);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        #[cfg(feature = "tweaker")]
        makepad_widgets_tweaker::link(vm);
        makepad_widgets::script_mod(vm);
        makepad_create::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Some(removed) = &self.view.workspace.removed {
            let elsewhere = match event {
                Event::KeyDown(k) if k.key_code == KeyCode::KeyZ && (k.modifiers.logo || k.modifiers.control)
                    && !k.modifiers.shift && !k.modifiers.alt => {
                    if !k.is_repeat { self.undo_removed_project(cx); }
                    return
                }
                Event::KeyDown(k) => !matches!(k.key_code, KeyCode::Logo | KeyCode::Control | KeyCode::Shift | KeyCode::Alt),
                Event::MouseUp(_) | Event::Scroll(_) | Event::TextInput(_) | Event::A11yAction(_) | Event::Drop(_) => true,
                Event::TouchUpdate(t) => t.touches.iter().any(|p| p.state == TouchState::Stop),
                _ => false,
            };
            // Handle the click against the rows the person saw, then dismiss its old Undo.
            if elsewhere { cx.action(DismissRemovedProject(removed.event)); }
        }
        if let Event::WindowGeomChange(change) = event {
            self.fit_window(cx, change.new_geom.inner_size.x, false);
        }
        if let Some(result) = self.clock_load.as_mut().and_then(|task| task.try_take()) {
            self.clock_load = None;
            if let Ok(clock) = result {
                self.clock = clock;
                self.refresh(cx);
            }
        }
        if self.presence_timer.is_event(event).is_some() {
            self.update_presence(cx);
            self.update_ai_commands(cx);
        }
        if matches!(event, Event::LiveEdit | Event::ScriptReapply) {
            let width = self.ui.window(cx, ids!(main_window)).get_inner_size(cx).x;
            self.fit_window(cx, width, true);
        }
        self.publish_event(cx, event);
        // The editor has a local typing buffer. An edit attempted after removal cannot
        // be flushed. Reattach the saved copy so the person can still switch or close.
        if self.document.as_ref().is_some_and(|d| d.dirty())
            && self.model.as_ref().is_some_and(|m| !m.state.active_member(&m.me())) {
            self.document = None;
            self.update_page(cx);
        }
        self.doc_event(cx, event);
        self.drain_port(cx, event);
        self.match_event(cx, event);
        // A queued widget action has lost its input provenance. Accept Confirm only while
        // dispatching the original local input that clicked it.
        let forget = self.view.connect.confirm_forget && self.ui.popover(cx, ids!(connections)).is_open();
        let confirm_button = if forget { Some(ids!(connect_devices_confirm)) }
            else if self.view.page == Page::Publish { Some(ids!(publish_confirm)) }
            else { None };
        if let Some(button) = confirm_button.filter(|_| cx.injected_by().is_none()
            && matches!(event, Event::MouseUp(_) | Event::KeyUp(_) | Event::TouchUpdate(_) | Event::A11yAction(_))) {
            let actions = cx.capture_actions(|cx| self.ui.handle_event(cx, event, &mut Scope::with_data(&mut self.view)));
            let confirm = self.ui.button(cx, button).clicked(&actions);
            cx.extend_actions(actions);
            if confirm {
                if forget { self.confirm_forget_devices(cx); } else { self.confirm_publish(cx); }
            }
        } else {
            self.ui.handle_event(cx, event, &mut Scope::with_data(&mut self.view));
        }
        // What this event changed locally goes to a linked peer.
        self.receiving_peer = true;
        self.link_event(cx);
        self.receiving_peer = false;
    }
}

#[cfg(test)]
mod tests {
    use super::split_at_ai;

    #[test]
    fn at_ai_is_a_prefix_word() {
        assert_eq!(split_at_ai("@ai look"), (true, "look".into()));
        assert_eq!(split_at_ai("  @AI\tlook "), (true, "look".into()));
        assert_eq!(split_at_ai("🙂 hello"), (false, "🙂 hello".into()));
        assert_eq!(split_at_ai("@aida is here"), (false, "@aida is here".into()));
        assert_eq!(split_at_ai("ask @ai later"), (false, "ask @ai later".into()));
    }
}
