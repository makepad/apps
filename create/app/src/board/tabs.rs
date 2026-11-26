//! A horizontal board list. Button actions carry the board id from the displayed tab.

use super::BoardPicker;
use makepad_widgets::event::TouchState;
use makepad_widgets::*;
use makepad_work_core::event::Id;

#[derive(Clone, Copy, Debug, PartialEq)]
enum TabButton {
    Choose(Id),
    Rename(Id),
    New,
}

impl TabButton {
    fn clicked(action: &WidgetAction) -> Option<Self> {
        if !matches!(action.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_))) { return None }
        action.data.as_ref()?.downcast_ref::<Self>().copied()
    }
}

pub enum TabAction {
    Choose(Id),
    New,
    Rename(Id, String),
    FocusCanvas,
}

struct Rename {
    board: Id,
    original: String,
    field: TextInputRef,
    focus: bool,
}

#[derive(Script, ScriptHook, Widget)]
pub struct BoardTabs {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] boards: Vec<Id>,
    #[rust] titles: Vec<String>,
    #[rust] current: Option<Id>,
    #[rust] writable: bool,
    #[rust] editing: Option<Rename>,
    #[rust] last_tab_click: Option<Id>,
}

impl BoardTabs {
    pub fn show(&mut self, cx: &mut Cx, picker: &BoardPicker, index: Option<usize>) {
        let current = index.map(|i| picker.boards[i]);
        if current == self.current && self.boards == picker.boards && self.titles == picker.labels { return }
        if self.current != current {
            self.editing = None;
            if let Some(i) = index { self.portal_list(cx, ids!(list)).keep_index_visible(cx, i); }
        }
        self.current = current;
        self.boards.clone_from(&picker.boards);
        self.titles.clone_from(&picker.labels);
        self.redraw(cx);
    }

    pub fn edit_context(&mut self, cx: &mut Cx, writable: bool) {
        if self.writable == writable { return }
        self.writable = writable;
        if !writable {
            self.editing = None;
            self.last_tab_click = None;
        }
        self.redraw(cx);
    }

    pub fn actions(&mut self, cx: &mut Cx, actions: &Actions) -> Vec<TabAction> {
        let mut result = Vec::new();
        if let Some(edit) = &self.editing {
            let cancel = edit.field.escaped(actions);
            let submit = edit.field.returned(actions).is_some();
            if cancel || submit || edit.field.key_focus_lost(actions) {
                let title = edit.field.text();
                if !cancel && title != edit.original { result.push(TabAction::Rename(edit.board, title)); }
                if cancel || submit { result.push(TabAction::FocusCanvas); }
                self.editing = None;
                self.redraw(cx);
            }
        }
        for action in actions.iter().filter_map(|a| a.as_widget_action()) {
            match TabButton::clicked(action) {
                Some(TabButton::Choose(id)) if self.boards.contains(&id) => result.push(TabAction::Choose(id)),
                Some(TabButton::New) if self.writable => result.push(TabAction::New),
                Some(TabButton::Rename(id)) if self.writable && self.current == Some(id) => {
                    let Some(i) = self.boards.iter().position(|board| *board == id) else { continue };
                    let list = self.portal_list(cx, ids!(list));
                    let field = list.item(cx, i, id!(Editing)).text_input(cx, ids!(title));
                    let original = self.titles[i].clone();
                    field.set_text(cx, &original);
                    self.editing = Some(Rename { board: id, original, field, focus: true });
                    list.keep_index_visible(cx, i);
                    self.redraw(cx);
                }
                _ => {}
            }
        }
        result
    }
}

impl Widget for BoardTabs {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { continue };
            list.set_item_range(cx, 0, self.boards.len() + 1);
            while let Some(i) = list.next_visible_item(cx) {
                if i == self.boards.len() {
                    let row = list.item(cx, i, id!(Add));
                    let add = row.button(cx, ids!(new_board));
                    add.set_action_data(TabButton::New);
                    add.set_enabled(cx, self.writable);
                    row.draw_all(cx, scope);
                } else if let Some(&board) = self.boards.get(i) {
                    if let Some(edit) = self.editing.as_mut().filter(|edit| edit.board == board) {
                        let row = list.item(cx, i, id!(Editing));
                        let field = row.text_input(cx, ids!(title));
                        if field.widget_uid() != edit.field.widget_uid() {
                            field.set_text(cx, &edit.field.text());
                            edit.field = field;
                        }
                        row.draw_all(cx, scope);
                        if edit.focus {
                            edit.field.take_key_focus_from_user(cx);
                            if let Some(mut input) = edit.field.borrow_mut() { input.select_all(cx); }
                            edit.focus = false;
                        }
                    } else {
                        let current = self.current == Some(board);
                        let row = list.item(cx, i, if current { id!(Current) } else { id!(Tab) });
                        let choose = row.button(cx, ids!(choose));
                        choose.set_text(cx, &self.titles[i]);
                        choose.set_action_data(TabButton::Choose(board));
                        if current {
                            let rename = row.button(cx, ids!(rename));
                            rename.set_visible(cx, self.writable);
                            rename.set_action_data(TabButton::Rename(board));
                        }
                        row.draw_all(cx, scope);
                    }
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let release = matches!(event, Event::MouseUp(_))
            || matches!(event, Event::TouchUpdate(e) if e.touches.iter().any(|t| matches!(t.state, TouchState::Stop | TouchState::Cancel)));
        if !self.writable || !release {
            self.view.handle_event(cx, event, scope);
            return
        }
        let previous = self.last_tab_click.take();
        let mut actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        for action in &mut actions {
            let Some(action) = action.downcast_mut::<WidgetAction>() else { continue };
            let Some(TabButton::Choose(board)) = TabButton::clicked(action) else { continue };
            let area = cx.widget_tree().widget(action.widget_uid).area();
            let Hit::FingerUp(up) = event.hits(cx, area) else { continue };
            if !up.was_tap() { continue }
            // Selecting a different tab replaces its button. Pair taps by board id.
            if up.tap_count >= 2 && previous == Some(board) {
                action.data = Some(std::sync::Arc::new(TabButton::Rename(board)));
            } else {
                self.last_tab_click = Some(board);
            }
        }
        cx.extend_actions(actions);
    }
}
