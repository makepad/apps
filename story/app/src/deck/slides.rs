//! The left column's tabs (Library, Slides, Files), the Slides tab's add
//! and right-click menus, and the right column's settings for the shown
//! slide. Every change is the same `DeckOp` the AI's slide tools run.
use super::*;

/// The left column's tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Nav {
    Library,
    #[default]
    Slides,
    Files,
}

impl Nav {
    const ALL: [Nav; 3] = [Nav::Library, Nav::Slides, Nav::Files];
}

impl DeckPage {
    /// Show `nav` in the left column.
    pub(super) fn set_nav(&mut self, cx: &mut Cx, ui: &WidgetRef, nav: Nav) {
        if nav != Nav::Slides {
            if let Some(mut sorter) = ui.widget(cx, ids!(deck_sorter)).borrow_mut::<DeckSorter>() { sorter.cancel_drag(cx); }
        }
        self.nav = nav;
        ui.widget(cx, ids!(deck_library)).set_visible(cx, nav == Nav::Library);
        ui.widget(cx, ids!(deck_slides)).set_visible(cx, nav == Nav::Slides);
        ui.widget(cx, ids!(deck_file_list)).set_visible(cx, nav == Nav::Files);
        if let Some(mut tabs) = ui.widget(cx, ids!(deck_nav_tabs)).borrow_mut::<CutSegmented>() {
            tabs.set_active(cx, Nav::ALL.iter().position(|tab| *tab == nav).unwrap_or(1));
        }
        if nav == Nav::Library { self.lib_dirty = true; }
        self.panels_seen = None;
        self.sync_columns(cx, ui);
        ui.redraw(cx);
    }

    /// The tabs, and the two top-bar buttons that open the left column on
    /// the library or on the slides (or close it when it already shows that).
    pub(super) fn nav_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        for action in actions.filter_widget_actions_cast::<SegmentAction>(ui.widget(cx, ids!(deck_nav_tabs)).widget_uid()) {
            if let SegmentAction::Selected(index) = action {
                if let Some(nav) = Nav::ALL.get(index) { self.set_nav(cx, ui, *nav); }
            }
        }
        for (toggle, library) in [(live_id!(deck_toggle_library), true), (live_id!(deck_toggle_sorter), false)] {
            if !ui.button(cx, &[toggle]).clicked(actions) { continue; }
            let strip = ui.widget(cx, ids!(deck_strip));
            let showing = strip.visible() && (self.nav == Nav::Library) == library;
            strip.set_visible(cx, !showing);
            if !showing {
                let nav = if library { Nav::Library } else if self.nav == Nav::Library { Nav::Slides } else { self.nav };
                self.set_nav(cx, ui, nav);
            }
            self.panels_seen = None;
            self.sync_columns(cx, ui);
        }
    }

    pub(super) fn close_slide_menus(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let menu = ui.menu_layer(cx, ids!(menus));
        if matches!(menu.open_owner(), Some(owner) if owner == live_id!(deck_slide_menu) || owner == live_id!(deck_add_menu)) { menu.close(cx); }
        self.slide_menu = None;
    }

    pub(super) fn slide_menu_open(&self, cx: &mut Cx, ui: &WidgetRef) -> bool {
        matches!(ui.menu_layer(cx, ids!(menus)).open_owner(), Some(owner) if owner == live_id!(deck_slide_menu) || owner == live_id!(deck_add_menu))
    }

    /// A slide row right-clicked at `abs`: what can be done to that slide.
    /// `index` is the slide count for the list's background below the last
    /// slide: then only the new-slide rows (after the last slide).
    pub(super) fn open_slide_menu(&mut self, cx: &mut Cx, ui: &WidgetRef, index: usize, abs: Vec2d) {
        let slides = self.doc.as_ref().map_or(0, |doc| doc.slides.len());
        if self.doc.is_none() || index > slides { return; }
        self.close_file_menu(cx, ui);
        self.close_slide_menus(cx, ui);
        let sketch = index < slides && self.doc.as_ref().is_some_and(|doc| doc.slides.get(index).is_some_and(|slide| slide.sketch.is_some()));
        let mut rows = vec![
            MenuRow::new(live_id!(deck_slide_new), "New slide"),
            MenuRow::new(live_id!(deck_slide_new_app), "New app slide…"),
        ];
        if sketch { rows.push(MenuRow::new(live_id!(deck_slide_fancy), "Fancy it up")); }
        if index < slides {
            rows.extend([
                MenuRow::separator(),
                MenuRow::new(live_id!(deck_slide_duplicate), "Duplicate"),
                MenuRow::new(live_id!(deck_slide_up), "Move up").enabled(index > 0),
                MenuRow::new(live_id!(deck_slide_down), "Move down").enabled(index + 1 < slides),
                MenuRow::separator(),
                MenuRow::new(live_id!(deck_slide_delete), "Delete").enabled(slides > 1).danger(true),
            ]);
        }
        self.slide_menu = Some(index);
        self.add_anchor = Rect { pos: abs, size: dvec2(0.0, 0.0) };
        ui.menu_layer(cx, ids!(menus)).open(cx, live_id!(deck_slide_menu), rows, Rect { pos: abs, size: dvec2(0.0, 0.0) }, MenuPlace::At);
    }

    /// "New app slide…": the apps to put on a new slide (after `add_after`).
    fn open_add_menu(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        self.close_file_menu(cx, ui);
        self.close_slide_menus(cx, ui);
        let rows: Vec<MenuRow> = app_choices().map(|entry| MenuRow::new(add_app_id(entry.id), &format!("App: {}", entry.label))).collect();
        let anchor = self.add_anchor;
        ui.menu_layer(cx, ids!(menus)).open(cx, live_id!(deck_add_menu), rows, anchor, MenuPlace::Below);
    }

    pub(super) fn slide_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let slides = self.doc.as_ref().map_or(0, |doc| doc.slides.len());
        let current = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current());
        if let Some(id) = makepad_widgets::menu::menu_picked(actions, live_id!(deck_add_menu)) {
            let cut = app_choices().find(|entry| add_app_id(entry.id) == id)
                .map(|entry| NewCut::App { app: entry.id.to_string(), size: makepad_story_deck::AppDecl::DEFAULT_SIZE, demo: Vec::new() });
            if let Some(cut) = cut {
                // After the slide the menu was opened on (the list's
                // background: the last); else after the shown slide.
                let after = match self.add_after.take() {
                    Some(after) => after,
                    None => current.filter(|index| *index < slides),
                };
                let op = DeckOp::AddSlide { after, name: self.fresh_slide_name(), cut };
                match self.submit_op(cx, op) {
                    Ok(()) => self.select = Some(after.map_or(slides, |index| index + 1)),
                    Err(error) => self.load_error = error,
                }
            }
        }
        if let Some(id) = makepad_widgets::menu::menu_picked(actions, live_id!(deck_slide_menu)) {
            // New slides: after the slide under the pointer, or after the
            // last from the background below the list.
            let at = self.slide_menu;
            let after = at.filter(|index| *index < slides).or(slides.checked_sub(1));
            if id == live_id!(deck_slide_new) {
                // A sketch slide, typed on at once (the first line its title).
                self.slide_menu = None;
                if let Err(error) = self.add_sketch_slide(cx, ui, after.unwrap_or(0)) { self.load_error = error; }
                return;
            }
            if id == live_id!(deck_slide_new_app) {
                self.slide_menu = None;
                self.add_after = Some(after);
                return self.open_add_menu(cx, ui);
            }
            if let Some(index) = self.slide_menu.take().filter(|index| *index < slides) {
                let made = match id {
                    id if id == live_id!(deck_slide_fancy) => Some(self.fancy_up(cx, ui, index)),
                    _ => None,
                };
                if let Some(made) = made {
                    if let Err(error) = made { self.load_error = error; }
                    return;
                }
                let (op, select) = match id {
                    id if id == live_id!(deck_slide_duplicate) => (DeckOp::DuplicateSlide { index, name: None }, index + 1),
                    id if id == live_id!(deck_slide_up) && index > 0 => (DeckOp::MoveSlide { from: index, to: index - 1 }, index - 1),
                    id if id == live_id!(deck_slide_down) && index + 1 < slides => (DeckOp::MoveSlide { from: index, to: index + 1 }, index + 1),
                    id if id == live_id!(deck_slide_delete) && slides > 1 => (DeckOp::DeleteSlide { index }, index.min(slides - 2)),
                    _ => return,
                };
                match self.submit_op(cx, op) {
                    Ok(()) => { self.select = Some(select); self.file_slide = None; }
                    Err(error) => self.load_error = error,
                }
            }
        }
        if !self.slide_menu_open(cx, ui) { self.slide_menu = None; }
        self.target_actions(cx, ui, actions);
    }

    /// The shown slide's speaking time, as the field shows it (unless it is
    /// being typed in).
    pub(super) fn sync_target(&mut self, cx: &mut Cx, ui: &WidgetRef, target: Option<f64>) {
        let input = ui.text_input(cx, ids!(deck_slide_target));
        if has_focus(cx, input.area()) { return; }
        let text = target.map(|seconds| { let seconds = seconds.round().max(0.0) as u64; format!("{}:{:02}", seconds / 60, seconds % 60) }).unwrap_or_default();
        input.set_text(cx, &text);
        ui.label(cx, ids!(deck_slide_target_note)).set_text(cx, "");
    }

    fn target_actions(&mut self, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
        let Some((text, _)) = ui.text_input(cx, ids!(deck_slide_target)).returned(actions) else { return };
        let Some(index) = ui.widget(cx, ids!(deck_view)).borrow::<DeckView>().and_then(|view| view.current()) else { return };
        let target = if text.trim().is_empty() { Ok(None) } else { duration::seconds(&text).map(Some) };
        match target {
            Ok(target) => {
                cx.set_key_focus(Area::Empty);
                let set = SlideSet { target_seconds: Some(target), ..SlideSet::default() };
                if let Err(error) = self.submit_op(cx, DeckOp::SetSlide { index, set }) { self.load_error = error; }
                self.synced = None;
            }
            Err(error) => ui.label(cx, ids!(deck_slide_target_note)).set_text(cx, &error),
        }
    }
}

/// The add menu's row for an app.
fn add_app_id(app: &str) -> LiveId {
    LiveId::from_str(&format!("deck_add_app_{app}"))
}
