//! The Board strip's stackup and base-rule form. Only drafts live here. Committed
//! values come from the board, including after undo, redo and edits by another pane.
use super::{BoardEditDone, BoardEditPress, BoardPane};
use crate::snapshot::next_tag;
use fab_elec_board::{Board, Finish};
use fab_elec_core::{to_mm, Nm};
use makepad_widgets_core::*;
use std::sync::Arc;

const FINISHES: &[(Finish, &str)] = &[
    (Finish::None, "None"), (Finish::Hasl, "HASL"), (Finish::LeadFreeHasl, "Lead-free HASL"),
    (Finish::Enig, "ENIG"), (Finish::Osp, "OSP"), (Finish::ImmersionSilver, "Immersion silver"),
    (Finish::ImmersionTin, "Immersion tin"),
];

#[derive(Clone, Copy, PartialEq)]
enum Setting { Factory, Thickness, Finish, Mask, Silk, Track, Clearance, Drill, Annular }

use Setting::*;
const FIELDS: [Setting; 9] = [Factory, Thickness, Setting::Finish, Mask, Silk, Track, Clearance, Drill, Annular];

impl Setting {
    fn key(self) -> &'static str {
        match self { Factory => "factory", Thickness => "thickness", Setting::Finish => "finish", Mask => "mask", Silk => "silk",
            Track => "track", Clearance => "clearance", Drill => "drill", Annular => "annular" }
    }

    fn rule(self) -> bool { matches!(self, Track | Clearance | Drill | Annular) }

    fn value(self, board: &Board, factory: Option<&str>) -> String {
        let stack = &board.stackup;
        let base = &board.rules.base;
        let mm = |v: Option<Nm>| v.map(|nm| to_mm(nm).to_string()).unwrap_or_default();
        let colour = |v: u32| if v & 255 == 255 { format!("#{:06x}", v >> 8) } else { format!("#{v:08x}") };
        match self {
            Factory => factory.unwrap_or_default().into(),
            Thickness => mm(Some(stack.thickness)),
            Setting::Finish => FINISHES.iter().find(|(f, _)| *f == stack.finish).unwrap().1.into(),
            Mask => colour(stack.mask_color), Silk => colour(stack.silk_color),
            Track => mm(base.track), Clearance => mm(base.clearance), Drill => mm(base.drill), Annular => mm(base.annular),
        }
    }

    fn row(self, cx: &Cx, panel: &WidgetRef) -> WidgetRef { panel.widget(cx, &[LiveId::from_str(self.key())]) }
}

#[derive(Default)]
struct Field {
    committed: String,
    draft: Option<String>,
    pending: Option<(u64, String)>,
    refusal: Option<String>,
}

impl Field {
    fn sync(&mut self, setting: Setting, board: &Board, factory: Option<&str>) {
        let value = setting.value(board, factory);
        if self.committed != value {
            self.committed = value;
            self.refusal = None;
        }
    }

    fn displayed(&self) -> &str {
        self.draft.as_deref().or_else(|| self.pending.as_ref().map(|p| p.1.as_str())).unwrap_or(&self.committed)
    }

    fn change(&mut self, text: String) {
        self.refusal = None;
        self.draft = Some(text);
    }

    // Enter and its subsequent focus loss consume the same draft only once.
    fn commit(&mut self, setting: Setting, board: &str) -> Option<BoardEditPress> {
        let text = self.draft.take()?;
        if text.trim() == self.displayed() { return None; }
        let tag = next_tag();
        self.pending = Some((tag, text.clone()));
        Some(BoardEditPress { tag, action: match setting { Factory => "elec.factory.set", s if s.rule() => "elec.rules.set", _ => "elec.stackup.set" },
            args: vec![("board".into(), board.into()), ((if setting == Factory { "profile" } else { setting.key() }).into(), text)] })
    }

    fn finish(&mut self, done: &BoardEditDone) -> bool {
        if self.pending.as_ref().is_none_or(|p| p.0 != done.tag) { return false; }
        self.pending = None;
        if self.draft.is_none() { self.refusal.clone_from(&done.refusal); }
        true
    }
}

#[derive(Default)]
pub(super) struct SettingsPanel {
    pub(super) open: bool,
    fields: [Field; FIELDS.len()],
    board: Option<Arc<Board>>,
    factory: Option<String>,
    profiles: Option<Arc<Vec<fab_elec_cam::profile_doc::ProfileDef>>>,
}

impl SettingsPanel {
    fn write(&self, cx: &mut Cx, panel: &WidgetRef, setting: Setting) {
        let field = &self.fields[setting as usize];
        let row = setting.row(cx, panel);
        let value = field.displayed();
        if setting == Factory {
            let profiles = self.profiles.as_deref().map(Vec::as_slice).unwrap_or_default();
            let index = profiles.iter().position(|p| p.name == value).unwrap_or(profiles.len());
            row.drop_down(cx, ids!(picker)).set_selected_item(cx, index);
        } else if setting == Setting::Finish {
            let picker = row.drop_down(cx, ids!(picker));
            if let Some(i) = FINISHES.iter().position(|(_, name)| *name == value) { picker.set_selected_item(cx, i); }
        } else {
            let input = row.widget(cx, ids!(value));
            if input.text() != value { input.set_text(cx, value); }
        }
        if matches!(setting, Mask | Silk) {
            row.widget(cx, ids!(swatch)).set_text(cx, value);
        }
        let chip = row.widget(cx, ids!(refusal));
        chip.widget(cx, ids!(reason)).set_text(cx, field.refusal.as_deref().unwrap_or_default());
        chip.set_visible(cx, field.refusal.is_some());
    }

    fn sync(&mut self, cx: &mut Cx, panel: &WidgetRef, board: &Arc<Board>, factory: Option<&str>) {
        let first = self.board.is_none();
        if first {
            Setting::Finish.row(cx, panel).drop_down(cx, ids!(picker)).set_labels(cx, FINISHES.iter().map(|(_, name)| name.to_string()).collect());
        }
        let board_changed = self.board.as_ref().is_none_or(|old| !Arc::ptr_eq(old, board));
        let profiles = cx.get_global_ref::<crate::ElecLive>().map(|live| &live.snap.profiles);
        let factory_changed = first || self.factory.as_deref() != factory || self.profiles.as_ref().map(Arc::as_ptr) != profiles.map(Arc::as_ptr);
        if !board_changed && !factory_changed { return; }
        if factory_changed {
            self.profiles = profiles.cloned();
            self.factory = factory.map(String::from);
            let defs = self.profiles.as_deref().map(Vec::as_slice).unwrap_or_default();
            let mut labels: Vec<_> = defs.iter().map(|p| match &p.tier {
                Some(tier) => format!("{} ({tier})", p.name), None => p.name.clone(),
            }).collect();
            if !defs.iter().any(|p| Some(p.name.as_str()) == factory) {
                labels.push(factory.unwrap_or("No factory").to_string());
            }
            Factory.row(cx, panel).drop_down(cx, ids!(picker)).set_labels(cx, labels);
        }
        self.board = Some(board.clone());
        for setting in FIELDS {
            self.fields[setting as usize].sync(setting, board, factory);
            self.write(cx, panel, setting);
        }
        let caps = factory.and_then(|name| self.profiles.as_ref()?.iter().find(|p| p.name == name)).map(|p| &p.caps);
        let base = &board.rules.base;
        for setting in [Track, Clearance, Drill, Annular] {
            let row = setting.row(cx, panel);
            let limit = row.widget(cx, ids!(limit));
            let warning = row.widget(cx, ids!(raised));
            let values = caps.map(|c| match setting {
                Track => (base.track, c.min_track), Clearance => (base.clearance, c.min_clearance),
                Drill => (base.drill, c.min_drill), Annular => (base.annular, c.min_annular), _ => unreachable!(),
            });
            let raised = values.is_some_and(|(v, floor)| v.is_some_and(|nm| nm < floor));
            if let Some((_, floor)) = values {
                let mm = fab_elec_cam::caps::limit_text(floor);
                if raised { warning.set_text(cx, &format!("checks use {mm} (factory)")); }
                else { limit.set_text(cx, &format!("factory {mm}")); }
            }
            warning.set_visible(cx, raised);
            limit.set_visible(cx, caps.is_some() && !raised);
        }
    }

    fn commit(&mut self, cx: &mut Cx, panel: &WidgetRef, setting: Setting, board: &str) {
        if let Some(press) = self.fields[setting as usize].commit(setting, board) { cx.action(press); }
        self.write(cx, panel, setting);
    }
}

impl BoardPane {
    fn settings_unit(&self, cx: &mut Cx) -> Option<(Arc<fab_elec_design::Design>, fab_elec_core::BoardId)> {
        Some((self.design(cx)?, self.st.unit?))
    }

    pub(super) fn toggle_settings(&mut self, cx: &mut Cx) {
        if self.st.settings.open { self.close_settings(cx); return; }
        let Some((design, id)) = self.settings_unit(cx) else { return };
        let Some(unit) = design.unit(id) else { return };
        let Some(board) = &unit.board else { return };
        self.st.settings.open = true;
        self.settings.set_visible(cx, true);
        self.st.settings.sync(cx, &self.settings, board, unit.factory.as_deref());
        self.redraw(cx);
    }

    fn close_settings(&mut self, cx: &mut Cx) {
        if let Some((design, id)) = self.settings_unit(cx) {
            if let Some(unit) = design.unit(id) {
                for setting in FIELDS {
                    if self.st.settings.fields[setting as usize].draft.is_some() {
                        self.st.settings.commit(cx, &self.settings, setting, &unit.name);
                    }
                }
            }
        }
        self.st.settings.open = false;
        self.settings.set_visible(cx, false);
        self.redraw(cx);
    }

    pub(super) fn settings_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        if let Event::Actions(actions) = event {
            for done in actions.iter().filter_map(|a| a.downcast_ref::<BoardEditDone>()) {
                for setting in FIELDS {
                    if self.st.settings.fields[setting as usize].finish(done) {
                        self.st.settings.write(cx, &self.settings, setting);
                        self.redraw(cx);
                    }
                }
            }
        }
        if !self.st.settings.open { return false; }
        let Some((design, id)) = self.settings_unit(cx) else { return false };
        let Some(unit) = design.unit(id) else { return false };
        if matches!(event, Event::Signal) {
            if let Some(board) = &unit.board { self.st.settings.sync(cx, &self.settings, board, unit.factory.as_deref()); }
        }
        let menu_had_pointer = [Factory, Setting::Finish].iter().any(|s| cx.sweep_lock_area() == Some(s.row(cx, &self.settings).widget(cx, ids!(picker)).area()));
        let local = cx.capture_actions(|cx| self.settings.handle_event(cx, event, scope));
        // Assistive edits send their widget actions directly through the window.
        let mut apply = |actions: &Actions| {
            for setting in FIELDS {
                let row = setting.row(cx, &self.settings);
                let field = &mut self.st.settings.fields[setting as usize];
                if setting == Factory {
                    if let Some(index) = row.drop_down(cx, ids!(picker)).changed(actions) {
                        if let Some(profile) = self.st.settings.profiles.as_ref().and_then(|p| p.get(index)) {
                            field.change(profile.name.clone());
                            self.st.settings.commit(cx, &self.settings, setting, &unit.name);
                        }
                    }
                    continue;
                }
                if setting == Setting::Finish {
                    if let Some(i) = row.drop_down(cx, ids!(picker)).changed(actions) {
                        field.change(FINISHES[i].1.into());
                        self.st.settings.commit(cx, &self.settings, setting, &unit.name);
                    }
                    continue;
                }
                let input = row.text_input(cx, ids!(value));
                if let Some(text) = input.changed(actions) {
                    field.change(text);
                    self.st.settings.write(cx, &self.settings, setting);
                }
                if input.returned(actions).is_some() || input.key_focus_lost(actions) {
                    self.st.settings.commit(cx, &self.settings, setting, &unit.name);
                }
            }
        };
        if !local.is_empty() { apply(&local); }
        if let Event::Actions(actions) = event { apply(actions); }
        let rect = self.settings.area().rect(cx);
        if menu_had_pointer && matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::Scroll(_)) { return true; }
        if let Event::MouseDown(e) = event {
            let button = self.tools.widget(cx, ids!(a_board)).area().rect(cx);
            if !rect.contains(e.abs) && !button.contains(e.abs) {
                self.close_settings(cx);
            }
        }
        match event {
            Event::MouseDown(e) => rect.contains(e.abs), Event::MouseMove(e) => rect.contains(e.abs),
            Event::MouseUp(e) => rect.contains(e.abs), Event::Scroll(e) => rect.contains(e.abs),
            _ => false,
        }
    }

    pub(super) fn draw_settings(&mut self, cx: &mut Cx2d, scope: &mut Scope, pane: Rect) {
        if !self.st.settings.open {
            if self.settings.visible() { self.settings.set_visible(cx, false); }
            return;
        }
        let Some((design, id)) = self.settings_unit(cx) else { return };
        let Some(unit) = design.unit(id) else { return };
        let Some(board) = &unit.board else { return };
        self.st.settings.sync(cx, &self.settings, board, unit.factory.as_deref());
        let anchor = self.tools.widget(cx, ids!(a_board)).area().rect(cx);
        let width = 320.0_f64.min((pane.size.x - 8.0).max(0.0));
        let height = self.settings.area().rect(cx).size.y.max(298.0);
        let x = (anchor.pos.x + anchor.size.x + 8.0).min(pane.pos.x + pane.size.x - width - 4.0).max(pane.pos.x);
        let y = anchor.pos.y.min(pane.pos.y + pane.size.y - height - 4.0).max(pane.pos.y);
        cx.widget_tree_insert_child(self.uid, live_id!(settings), self.settings.clone());
        let mut walk = self.settings.walk(cx).with_abs_pos(dvec2(x, y));
        walk.width = Size::Fixed(width);
        self.settings.draw_walk_all(cx, scope, walk);
    }
}

