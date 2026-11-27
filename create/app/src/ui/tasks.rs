use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.BodyNote = Label{
        width: Fill height: Fit padding: 0 flow: Right{wrap: true}
        draw_text +: {color: palette.muted text_style: theme.font_regular{font_size: 9}}
    }
    mod.widgets.ProjectDot = RoundedView{width: 9 height: 9 draw_bg +: {color: palette.accent border_radius: 1.5}}
    mod.widgets.ProjectHeading = RailText{draw_text.text_style: theme.font_bold{font_size: 10.5}}
    mod.widgets.SeatIcons = View{
        width: Fit height: Fit spacing: 4
        you := SeatBadge{visible: false}
        your_ai := AiBadge{visible: false}
        peer := PeerBadge{visible: false}
        peer_ai := PeerAiBadge{visible: false}
    }
    mod.widgets.TaskCardBody = #(TaskCardBody::register_widget(vm))
    mod.widgets.TaskBoardBase = #(TaskBoard::register_widget(vm))
    let TaskCard = View{
        width: Fill height: Fit padding: Inset{bottom: 7}
        face := RoundedView{
            width: Fill height: Fit flow: Down spacing: 2 padding: 7
            draw_bg +: {color: palette.card border_color: palette.line border_size: 1 border_radius: 4}
            controls := View{
                width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 3
                number := Kicker{width: Fill draw_text.text_style: theme.font_code{font_size: 7.5}}
                authors := SeatIcons{}
                more := DropDown{
                    width: 26 height: 20 margin: 0 padding: 0 align: Center
                    a11y: {label: "Task menu"}
                    popup_menu_position: #(makepad_widgets::drop_down::PopupMenuPosition::BelowInput)
                    popup_menu: PopupMenu{width: Fit menu_item: PopupMenuItem{
                        draw_bg +: {mark_color: #0000 mark_color_active: #0000 mark_color_disabled: #0000}
                    }}
                    // Hover fills the dots' background. Keyboard focus outlines it.
                    draw_bg +: {
                        keyboard_focus: instance(0.0)
                        color: palette.card color_hover: palette.line color_down: palette.sel
                        border_color_focus: palette.accent
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, 3.0)
                            let hover_color = self.color_hover.mix(self.color_down, self.down)
                            sdf.fill_keep(self.color.mix(hover_color, self.hover * (1.0 - self.disabled)))
                            if self.keyboard_focus > 0.5 { sdf.stroke(self.border_color_focus, 0.75) }
                            return sdf.result
                        }
                    }
                    draw_text +: {color: palette.text color_focus: palette.text}
                }
            }
            title := RailText{width: Fill flow: Right{wrap: true} draw_text.text_style: theme.font_bold{font_size: 9.375}
                a11y: {label_for: "controls.more"}
            }
            proposal := View{
                visible: false width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 6
                BodyNote{width: Fit text: "Proposed"}
                accept := PanelButton{width: Fit text: "Accept"}
                dismiss := PanelButton{width: Fit text: "Dismiss"}
            }
            merge_here := PanelButton{visible: false width: Fill text: "Merge here"}
        }
    }
    let Cards = mod.widgets.CreateTaskListBase{
        width: Fill height: Fill
        list := ReorderList{
            width: Fill height: Fill draw_indicator.color: palette.faint
            TaskRow := mod.widgets.TaskCardBody{width: Fill height: Fit card := TaskCard{}}
        }
    }
    let Column = RoundedView{
        width: Fill height: Fill flow: Down padding: 8 spacing: 7
        draw_bg +: {color: palette.panel border_color: palette.line border_size: 1 border_radius: 4.5}
        heading := View{
            width: Fill height: Fit flow: Right align: Align{y: 0.5}
            padding: Inset{left: 3 right: 3 top: 2}
            state := Kicker{width: Fill}
            count := Kicker{text: "0"}
        }
        PanelLine{}
    }
    let BoardColumn = Column{
        empty := BodyNote{margin: Inset{left: 3 top: 2}}
        cards := Cards{}
    }
    let CaptureBar = #(crate::record::capture::bar_template(vm))
    let Capture = Tip{
        chip := RoundedView{
            width: Fit height: 29 flow: Right spacing: 6 align: Center padding: Inset{left: 9 right: 9}
            draw_bg +: {color: palette.panel2 border_size: 1 border_color: palette.line border_radius: 4}
            control := PanelButton{
                height: Fill padding: 0 text: "" draw_bg.border_size: 0
                draw_bg.color: palette.panel2 draw_bg.color_disabled: palette.panel2 draw_text.color: palette.text
            }
            key := RoundedView{
                width: Fit height: 16 padding: Inset{left: 4 right: 4} align: Center
                draw_bg +: {color: palette.panel2 border_color: palette.line border_size: 1 border_radius: 2}
                keycap := Kicker{}
            }
        }
    }
    mod.widgets.TaskBoard = mod.widgets.TaskBoardBase{
        visual: View{width: Fit height: Fit card := TaskCard{}}
        width: Fill height: Fill flow: Down
        SolidView{
            width: Fill height: Fit flow: Right align: Align{y: 0.5} spacing: 10
            padding: Inset{left: 12 right: 12 top: 9 bottom: 9}
            draw_bg.color: palette.panel
            View{
                width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 10
                ProjectDot{}
                project_name := ProjectHeading{}
                project_goal := BodyNote{width: Fit}
            }
        }
        PanelLine{}
        merge_hint := BodyNote{visible: false padding: Inset{left: 12 top: 6} text: "Choose the task to keep · Escape cancels"}
        proposals := View{
            visible: false width: Fill height: Fit flow: Right{wrap: true} align: Align{y: 0.5} spacing: 8
            padding: Inset{left: 12 right: 12 top: 6}
            proposals_note := BodyNote{width: Fit}
            accept_all := PanelButton{width: Fit text: "Accept all"}
            dismiss_all := PanelButton{width: Fit text: "Dismiss all"}
        }
        // The board sits in the capture bar's body: the window list it adds after the columns
        // then takes only the height the board gives up, so it opens above the chips and scrolls.
        capture_status := CaptureBar{
            body +: {
                columns := View{
                    width: Fill height: Fill flow: Right padding: 11 spacing: 9
                    first_pair := View{
                        width: Fill height: Fill flow: Right spacing: 9
                        open := Column{
                            heading +: {state +: {text: "OPEN"}}
                            task_input := PanelInput{empty_text: "Add a task…" a11y: {label: "New task title"}}
                            empty := BodyNote{text: "No open tasks yet."}
                            cards := Cards{}
                        }
                        doing := BoardColumn{cards +: {column: 1} heading +: {state +: {text: "DOING"}} empty +: {text: "Nothing in progress yet."}}
                    }
                    second_pair := View{
                        width: Fill height: Fill flow: Right spacing: 9
                        review := BoardColumn{cards +: {column: 2} heading +: {state +: {text: "REVIEW"}} empty +: {text: "Nothing in review yet."}}
                        done := BoardColumn{cards +: {column: 3} heading +: {state +: {text: "DONE"}} empty +: {text: "No completed tasks yet."}}
                    }
                }
                PanelLine{}
            }
        }
        capture := SolidView{
            width: Fill height: Fit flow: Right{wrap: true} spacing: 8 align: Align{y: 0.5}
            padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
            draw_bg.color: palette.panel
            snap := Capture{text: "Take a picture of a window" chip +: {control +: {text: "Snap"} key +: {keycap +: {text: #(if cfg!(target_os = "macos") { "⌘⇧1" } else { "Ctrl+Shift+1" })}}}}
            clip := Capture{text: "Record a window" chip +: {control +: {text: "Clip"} key +: {keycap +: {text: #(if cfg!(target_os = "macos") { "⌘⇧2" } else { "Ctrl+Shift+2" })}}}}
            capture_task := PanelButton{text: "Task"}
        }
    }
}

use crate::{lists::{CreateTaskList, Page, ViewData}, model::Action, view::{Card, DecideProposed, Project}};
use makepad_town_core::event::Id;
use makepad_widgets::{drag_visual::CxDragVisualExt, drop_down::DropDownAction, event::TouchState};

const STATUSES: [&str; 4] = ["open", "doing", "review", "done"];
const MOVE_LABELS: [&str; 4] = ["Move to Open", "Move to Doing", "Move to Review", "Move to Done"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Row { project: Id, task: Id, column: usize }

#[derive(Clone, Copy, Debug)]
pub(crate) struct Press { row: Row, index: usize, window: WindowId, abs: DVec2, rect: Rect }

#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum TaskMode {
    #[default]
    Idle,
    Carry { press: Press, moving: bool },
    Merge(Row),
}

impl TaskMode {
    pub(crate) fn source_slot(&self, project: Id, column: usize) -> Option<usize> {
        match self { Self::Carry { press, moving: true } if (press.row.project, press.row.column) == (project, column) => Some(press.index), _ => None }
    }

    fn row(self) -> Option<Row> {
        match self { Self::Idle => None, Self::Carry { press, .. } => Some(press.row), Self::Merge(row) => Some(row) }
    }
}

/// The board asks the window to apply existing actions to the project whose rows were displayed.
#[derive(Clone, Debug)]
pub struct TaskEdits { pub project: Id, pub actions: Vec<Action> }

fn drop_actions(task: Id, source: usize, destination: usize, slot: usize, cards: impl Iterator<Item = Id>) -> Vec<Action> {
    // The slot indexes the displayed destination with the carried card already removed.
    let mut after = None;
    let mut before = None;
    let mut own_slot = None;
    let mut index = 0;
    for id in cards {
        if id == task { own_slot = Some(index); continue }
        if index < slot { after = Some(id); } else if index == slot { before = Some(id); }
        index += 1;
    }
    if slot > index || source == destination && own_slot == Some(slot) { return Vec::new() }
    let mut actions = Vec::new();
    if source != destination { actions.push(Action::SetTaskStatus { task, status: STATUSES[destination].into() }); }
    actions.push(Action::MoveTask { task, before, after });
    actions
}

fn fill_card(cx: &mut Cx, view: &WidgetRef, card: &Card) {
    view.label(cx, ids!(number)).set_text(cx, &card.number);
    view.label(cx, ids!(title)).set_text(cx, &card.title);
    crate::lists::seat_icon(cx, &view.widget(cx, ids!(authors)), card.seat, &card.author_name);
    view.widget(cx, ids!(proposal)).set_visible(cx, card.proposed);
}

#[derive(Script, ScriptHook, Widget)]
pub struct TaskCardBody {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] row: Option<Row>,
    #[rust] writable: bool,
    #[rust] proposed: bool,
    #[rust] target_title: String,
    #[rust] index: usize,
}

impl TaskCardBody {
    pub(crate) fn bind(&mut self, cx: &mut Cx, project: Id, column: usize, index: usize, card: &Card, data: &ViewData) {
        let row = Row { project, column, task: card.id };
        let face = self.view.widget(cx, ids!(face));
        fill_card(cx, &face, card);
        let more = face.drop_down(cx, ids!(more));
        let mut target = face.button(cx, ids!(merge_here));
        if self.row != Some(row) {
            if self.row.is_some() {
                if let Some(mut menu) = more.borrow_mut() { menu.set_closed(cx); }
            }
            let mut labels: Vec<_> = MOVE_LABELS.iter().enumerate().filter(|(i, _)| *i != column).map(|(_, s)| s.to_string()).collect();
            labels.push("Merge into…".into());
            more.set_labels(cx, labels);
            more.set_face_labels(cx, vec!["⋯".into(); 4]);
            more.set_action_data(row);
            target.set_action_data(row);
            self.row = Some(row);
        }
        self.proposed = card.proposed;
        face.button(cx, ids!(accept)).set_action_data(DecideProposed { project, task: Some(card.id), accept: true });
        face.button(cx, ids!(dismiss)).set_action_data(DecideProposed { project, task: Some(card.id), accept: false });
        if self.target_title != card.title {
            self.target_title.clone_from(&card.title);
            let name = cx.with_vm(|vm| vm.new_string(&format!("Merge into {}", card.title)));
            script_apply_eval!(cx, target, {a11y_name: #(name)});
        }
        let is_target = matches!(data.task_mode, TaskMode::Merge(source) if source.project == project && source.task != card.id);
        target.set_visible(cx, is_target);
        self.index = index;
        self.writable = data.workspace.writable;
        if more.disabled(cx) == self.writable { more.set_disabled(cx, !self.writable); }
    }
}

impl Widget for TaskCardBody {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // The menu and merge button get first refusal, including keyboard and accessibility input.
        self.view.handle_event(cx, event, scope);
        let Some(row) = self.row.filter(|_| self.writable) else { return };
        if !matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseLeave(_)
            | Event::ClearHover | Event::TouchUpdate(_)) { return }
        let face = self.view.widget(cx, ids!(face));
        let more = face.drop_down(cx, ids!(more));
        let point = match event {
            Event::MouseMove(m) => Some(m.abs),
            Event::MouseDown(m) => Some(m.abs),
            _ => None,
        };
        let buttons = [more.area(), face.button(cx, ids!(accept)).area(), face.button(cx, ids!(dismiss)).area()];
        if point.is_some_and(|p| buttons.iter().enumerate().any(|(i, area)| (i == 0 || self.proposed) && area.clipped_rect(cx).contains(p))) {
            if matches!(event, Event::MouseMove(_)) { cx.set_cursor(MouseCursor::Arrow); }
            return
        }
        // During capture the old menu can miss FingerHoverOut, including when its row is parked.
        if !matches!(event, Event::TouchUpdate(_)) {
            if let Some(mut menu) = more.borrow_mut() {
                if !menu.animator_in_state(cx, ids!(hover.off)) { menu.animator_play(cx, ids!(hover.off)); }
            }
        }
        match event.hits(cx, face.area()) {
            Hit::FingerDown(d) if d.is_primary_hit() => {
                cx.claim_finger_drag(d.digit_id);
                cx.widget_action(self.widget_uid(), Press { row, index: self.index, window: d.window_id, abs: d.abs, rect: self.view.area().rect(cx) });
            }
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Hand),
            _ => {}
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct TaskBoard {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[live] visual: ScriptObjectRef,
}

impl TaskBoard {
    fn column(&self, cx: &Cx, index: usize) -> WidgetRef {
        self.view.widget(cx, [ids!(first_pair.open), ids!(first_pair.doing), ids!(second_pair.review), ids!(second_pair.done)][index])
    }

    fn preview(&self, cx: &mut Cx, press: Press, point: Option<DVec2>, project: &Project) -> Option<(usize, usize)> {
        let mut destination = None;
        for (i, cards) in project.columns.iter().enumerate() {
            let column = self.column(cx, i);
            let rows = column.widget(cx, ids!(cards));
            let list_ref = rows.reorder_list(cx, ids!(list));
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            let slot = point.filter(|p| column.area().clipped_rect(cx).contains(*p)).map(|p| {
                if i == press.row.column && press.rect.contains(p) { return press.index }
                let rect = list.area().rect(cx);
                let raw = if p.y < rect.pos.y { list.first_id() } else {
                    list.row_at(cx, dvec2(rect.pos.x + rect.size.x * 0.5, p.y.min(rect.pos.y + rect.size.y))).unwrap_or(cards.len())
                };
                // A release may precede the first paint that closes the source's place.
                let omitted = rows.borrow::<CreateTaskList>().and_then(|r| r.omitted());
                let source = i == press.row.column;
                let slot = raw - usize::from(source && omitted.is_none() && press.index < raw);
                slot.min(cards.len().saturating_sub(usize::from(source)))
            });
            column.widget(cx, ids!(empty)).set_visible(cx, cards.is_empty() && slot.is_none());
            list.set_external_gap_height(cx, slot.map(|_| press.rect.size.y));
            list.set_external_gap(cx, slot);
            if let Some(slot) = slot { destination = Some((i, slot)); }
        }
        destination
    }

    fn reset(&mut self, cx: &mut Cx, data: &mut ViewData) {
        if let TaskMode::Carry { .. } = data.task_mode {
            if let Some(visual) = cx.drag_visual() { visual.set_visible(cx, false); }
            for i in 0..4 {
                let column = self.column(cx, i);
                let empty = data.workspace.selected().is_none_or(|project| project.columns[i].is_empty());
                column.widget(cx, ids!(empty)).set_visible(cx, empty);
                if let Some(mut list) = column.reorder_list(cx, ids!(cards.list)).borrow_mut() {
                    list.set_external_gap(cx, None);
                    list.set_external_gap_height(cx, None);
                }
            }
        }
        data.task_mode = TaskMode::Idle;
        self.view.widget(cx, ids!(merge_hint)).set_visible(cx, false);
        self.view.redraw(cx);
    }

    fn pressed(&mut self, cx: &mut Cx, data: &mut ViewData, press: Press) {
        let Some(project) = data.workspace.selected().filter(|p| p.id == press.row.project && data.workspace.writable) else { return };
        let Some(card) = project.columns[press.row.column].get(press.index).filter(|c| c.id == press.row.task) else { return };
        match data.task_mode {
            TaskMode::Merge(source) => self.merge(cx, data, source, press.row),
            TaskMode::Idle => {
                let visual = cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.visual.as_object().into()));
                let body = visual.view(cx, ids!(card));
                if let Some(mut body) = body.borrow_mut() { body.walk.width = Size::Fixed(press.rect.size.x); }
                fill_card(cx, &body, card);
                body.drop_down(cx, ids!(more)).set_face_labels(cx, vec!["⋯".into()]);
                // The original stays under the held pointer until the window paints its overlay on the first move.
                data.task_mode = TaskMode::Carry { press, moving: false };
                let payload = DragItem::String { internal_id: Some(live_id!(town_task_card)), value: makepad_town_core::hex(&card.id) };
                cx.set_cursor(MouseCursor::Hand);
                cx.start_dragging_with(vec![payload], visual, press.rect.pos - press.abs);
                self.view.redraw(cx);
            }
            TaskMode::Carry { .. } => {}
        }
    }

    fn merge(&mut self, cx: &mut Cx, data: &mut ViewData, source: Row, target: Row) {
        if source.project == target.project && source.task != target.task {
            let action = Action::MergeTasks { into: target.task, from: vec![source.task], title: None, text: None };
            cx.widget_action(self.widget_uid(), TaskEdits { project: source.project, actions: vec![action] });
        }
        self.reset(cx, data);
    }

    fn choices(&mut self, cx: &mut Cx, data: &mut ViewData, actions: &Actions) {
        for action in actions.iter().filter_map(|a| a.as_widget_action()) {
            let Some(&row) = action.data.as_ref().and_then(|d| d.downcast_ref::<Row>()) else { continue };
            if !data.workspace.writable || data.workspace.current != Some(row.project) { continue }
            if let Some(DropDownAction::Select(choice)) = action.action.downcast_ref::<DropDownAction>() {
                if let Some(column) = (0..4).filter(|&i| i != row.column).nth(*choice) {
                    let status = Action::SetTaskStatus { task: row.task, status: STATUSES[column].into() };
                    cx.widget_action(self.widget_uid(), TaskEdits { project: row.project, actions: vec![status] });
                } else if *choice == 3 {
                    data.task_mode = TaskMode::Merge(row);
                    self.view.widget(cx, ids!(merge_hint)).set_visible(cx, true);
                    self.view.redraw(cx);
                }
            } else if matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) {
                if let TaskMode::Merge(source) = data.task_mode { self.merge(cx, data, source, row); }
            }
        }
    }
}

impl TaskBoardRef {
    pub fn reconcile(&self, cx: &mut Cx, data: &mut ViewData) {
        let Some(row) = data.task_mode.row() else { return };
        let valid = data.page == Page::Tasks && data.workspace.writable && data.workspace.selected().is_some_and(|p|
            p.id == row.project && match data.task_mode {
                TaskMode::Carry { press, .. } => p.columns[row.column].get(press.index).is_some_and(|c| c.id == row.task),
                _ => p.columns[row.column].iter().any(|c| c.id == row.task),
            });
        if !valid {
            if let Some(mut board) = self.borrow_mut() { board.reset(cx, data); }
        }
    }
}

impl Widget for TaskBoard {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !self.visible() { return }
        let down = matches!(event, Event::MouseDown(d) if d.button.is_primary()) ||
            matches!(event, Event::TouchUpdate(t) if t.touches.iter().any(|f| f.state == TouchState::Start));
        let activating = matches!(event, Event::A11yAction(a) if a.action == a11y_tree::Actions::PRESS || a.action == a11y_tree::Actions::EXPAND);
        if down || activating {
            let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
            if let Some(data) = scope.data.get_mut::<ViewData>() {
                if let Some(press) = actions.iter().filter_map(|a| a.as_widget_action()).find_map(|a| a.action.downcast_ref::<Press>()) {
                    self.pressed(cx, data, *press);
                } else if matches!(data.task_mode, TaskMode::Merge(_)) && !actions.iter().filter_map(|a| a.as_widget_action()).any(|a|
                    a.data.as_ref().is_some_and(|d| d.downcast_ref::<Row>().is_some()) && matches!(a.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Pressed(_) | ButtonAction::Clicked(_)))) {
                    self.reset(cx, data);
                }
            }
            cx.extend_actions(actions);
        } else {
            self.view.handle_event(cx, event, scope);
        }
        let Some(data) = scope.data.get_mut::<ViewData>() else { return };
        if let Event::Actions(actions) = event { self.choices(cx, data, actions); }
        if data.task_mode.row().is_none() { return }
        // Recycling a row cancels its capture while the window's drag still owns the press.
        if matches!(event, Event::WindowLostFocus(_) | Event::DragEnd)
            || matches!(event, Event::FingerCancel(c) if cx.fingers.press_taken_away(c.digit_id))
            || matches!(event, Event::KeyDown(k) if k.key_code == KeyCode::Escape) {
            self.reset(cx, data);
            return
        }
        let TaskMode::Carry { press, moving } = data.task_mode else { return };
        let (point, window, items) = match event {
            Event::Drag(d) => (d.abs, d.window_id, &d.items),
            Event::Drop(d) => (d.abs, d.window_id, &d.items),
            Event::MouseLeave(_) => {
                if let Some(project) = data.workspace.selected() { self.preview(cx, press, None, project); }
                return
            }
            _ => return,
        };
        let Some(project) = data.workspace.selected().filter(|p| p.id == press.row.project) else { return };
        if !items.iter().any(|item| matches!(item, DragItem::String {internal_id: Some(id), value}
            if *id == live_id!(town_task_card) && *value == makepad_town_core::hex(&press.row.task))) { return }
        if matches!(event, Event::Drag(_)) {
            if window == press.window && cx.mouse_cursor() != MouseCursor::Hand { cx.set_cursor(MouseCursor::Hand); }
            if !moving {
                data.task_mode = TaskMode::Carry { press, moving: true };
                self.view.redraw(cx);
            }
        }
        let destination = self.preview(cx, press, (window == press.window).then_some(point), project);
        match event {
            Event::Drag(d) if destination.is_some() => *d.response.lock().unwrap_or_else(|e| e.into_inner()) = DragResponse::Move,
            Event::Drop(d) => {
                if let Some((column, slot)) = destination {
                    *d.handled.lock().unwrap_or_else(|e| e.into_inner()) = true;
                    if point != press.abs {
                        let actions = drop_actions(press.row.task, press.row.column, column, slot, project.columns[column].iter().map(|c| c.id));
                        if !actions.is_empty() { cx.widget_action(self.widget_uid(), TaskEdits { project: project.id, actions }); }
                    }
                }
                self.reset(cx, data);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_menu_hover_focus_and_carry_follow_the_pointer() {
        use crate::{model::{tests::ShareHomes, Model, Origin}, view::Workspace};
        use makepad_platform::studio::{RemoteMouseDown, RemoteMouseMove, RemoteMouseUp, StudioToApp};
        use makepad_widgets::{cx_draw::CxDraw, makepad_draw::overlay::Overlay};
        use std::{cell::RefCell, rc::Rc};

        let home = ShareHomes::new("card-pointer");
        let mut model = Model::open(&home.0).unwrap().0;
        let [first, last] = ["Check the frame", "Print the panel"].map(|title|
            model.act(Action::AddTask { title: title.into(), text: String::new() }, Origin::Ui).unwrap());
        let board = WidgetRef::empty();
        let data = Rc::new(RefCell::new(ViewData {
            workspace: Workspace::read(&model, false, &Default::default()), page: Page::Tasks, ..Default::default()
        }));
        let target = board.clone();
        let state = data.clone();
        let mut cx = Cx::new(Box::new(move |cx, event| {
            if let Event::Actions(actions) = event {
                for edit in actions.iter().filter_map(|a| a.as_widget_action()).filter_map(|a| a.action.downcast_ref::<TaskEdits>()) {
                    for action in &edit.actions { model.act(action.clone(), Origin::Ui).unwrap(); }
                    state.borrow_mut().workspace = Workspace::read(&model, false, &Default::default());
                }
            }
            target.handle_event(cx, event, &mut Scope::with_data(&mut *state.borrow_mut()));
        }));
        cx.init_cx_os();
        cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::script_mod(vm);
            let root = script_eval!(vm, {use mod.widgets.* TaskBoard{}});
            let mut board = board.clone();
            board.script_apply(vm, &Apply::New, &mut Scope::empty(), root);
        });
        let size = dvec2(960., 600.);
        let pass = DrawPass::new(&mut cx);
        pass.set_size(&mut cx, size);
        let mut list = DrawList2d::new(&mut cx);
        let overlay = cx.with_vm(Overlay::script_new);
        let event = DrawEvent::default();
        let mut draw = |cx: &mut Cx| {
            let mut draw = CxDraw::new(cx, &event);
            let mut frame = Cx2d::new(&mut draw);
            frame.begin_pass(&pass, None);
            list.begin_always(&mut frame);
            overlay.begin(&mut frame);
            frame.begin_root_turtle(size, Layout::flow_down());
            board.draw_all(&mut frame, &mut Scope::with_data(&mut *data.borrow_mut()));
            frame.end_pass_sized_turtle();
            overlay.end(&mut frame);
            list.end(&mut frame);
            frame.end_pass(&pass);
        };
        draw(&mut cx);
        let open = board.reorder_list(&cx, ids!(open.cards.list));
        let row = |i| open.borrow().unwrap().get_item(i).expect("Open drew both tasks").1;
        let menu = row(0).drop_down(&cx, ids!(more));
        let dots = menu.area().rect(&cx).center();
        let body = row(1).area().rect(&cx).pos + dvec2(12., 12.);
        let gap = board.widget(&cx, ids!(first_pair.doing)).area().rect(&cx).center();
        let send = |cx: &mut Cx, msg| cx.dispatch_studio_msg(msg, WindowId(1, 1), DVec2::default());
        let motion = |cx: &mut Cx, p: DVec2| { send(cx, StudioToApp::MouseMove(RemoteMouseMove { x: p.x, y: p.y, ..Default::default() })); };
        let down = |cx: &mut Cx, p: DVec2| { send(cx, StudioToApp::MouseDown(RemoteMouseDown { x: p.x, y: p.y, button_raw_bits: MouseButton::PRIMARY.bits(), ..Default::default() })); };
        let up = |cx: &mut Cx, p: DVec2| { send(cx, StudioToApp::MouseUp(RemoteMouseUp { x: p.x, y: p.y, button_raw_bits: MouseButton::PRIMARY.bits(), ..Default::default() })); };

        motion(&mut cx, dots);
        assert!(menu.borrow().unwrap().animator_in_state(&cx, ids!(hover.on)), "entering the dots highlights the menu");
        assert_eq!(cx.mouse_cursor(), MouseCursor::Arrow, "the dots use the arrow");
        down(&mut cx, dots);
        up(&mut cx, dots);
        send(&mut cx, StudioToApp::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() }));
        motion(&mut cx, body);
        assert!(menu.borrow().unwrap().animator_in_state(&cx, ids!(hover.off)), "leaving a recently pressed menu clears its hover");
        assert!(cx.has_key_focus(menu.area()), "the menu keeps keyboard focus after the pointer leaves");
        assert_eq!(cx.mouse_cursor(), MouseCursor::Hand, "the card body asks for a hand");
        // Remote input can press the next card without an intervening hover move.
        motion(&mut cx, dots);
        down(&mut cx, body);
        assert_eq!(cx.mouse_cursor(), MouseCursor::Hand, "pressing the card picks it up with a hand");
        motion(&mut cx, gap);
        draw(&mut cx);
        assert_eq!(open.borrow().unwrap().range_end(), 1, "lifting the last Open card closes its row");
        // A second move lets the normal event loop deliver the row cancellation queued by the draw.
        motion(&mut cx, gap + dvec2(1., 0.));
        assert!(matches!(data.borrow().task_mode, TaskMode::Carry { press, moving: true } if press.row.task == last),
            "the board still carries the last task after the pressed row is recycled");
        assert!(menu.borrow().unwrap().animator_in_state(&cx, ids!(hover.off)), "a captured move away from the menu also clears its hover");
        assert_eq!(cx.mouse_cursor(), MouseCursor::Hand, "the carried task keeps the hand over an empty column");
        up(&mut cx, gap);
        let state = data.borrow();
        let columns = &state.workspace.selected().unwrap().columns;
        assert!(columns[0].iter().map(|c| c.id).eq([first]), "the other task stays in Open");
        assert!(columns[1].iter().map(|c| c.id).eq([last]), "dropping the recycled row's task places it in Doing");
    }

    #[test]
    fn drop_slots_name_the_neighbours_after_lifting_the_card() {
        let [a, b, c] = [[1; 32], [2; 32], [3; 32]];
        let moved = |before, after| Action::MoveTask { task: b, before, after };
        let same = |slot| drop_actions(b, 0, 0, slot, [a, b, c].into_iter());
        assert_eq!(same(0), [moved(Some(a), None)], "moving upward names the first remaining card");
        assert_eq!(same(2), [moved(None, Some(c))], "moving down past the last card excludes the source");
        assert!(same(1).is_empty(), "returning to the original neighbours writes nothing");
        for (slot, before, after) in [(0, Some(a), None), (1, Some(c), Some(a)), (2, None, Some(c))] {
            assert_eq!(drop_actions(b, 0, 1, slot, [a, c].into_iter()),
                [Action::SetTaskStatus { task: b, status: "doing".into() }, moved(before, after)],
                "cross-column slot {slot} sets the status before the order between its neighbours");
        }
        assert_eq!(drop_actions(b, 0, 3, 0, std::iter::empty()),
            [Action::SetTaskStatus { task: b, status: "done".into() }, moved(None, None)], "an empty column has one slot");
    }
}
