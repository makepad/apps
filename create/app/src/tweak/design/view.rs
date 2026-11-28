//! Structural controls shared by the live-app and board designers.
use super::super::{BakeScope, DesignPlace, DesignRequest, StepDir};
use makepad_strict_json::Value;
use makepad_widgets::*;
use std::collections::BTreeMap;

pub(in crate::tweak) fn strip_value(vm: &mut ScriptVm) -> ScriptValue {
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*
        let Small = PanelButton{width: Fit height: 20 min_height: 0 padding: Inset{left: 5 right: 5}
            draw_text.text_style.font_size: 8}
        View{width: Fill height: Fit flow: Right{wrap: true} spacing: 3
            design_add := Small{text: "+" a11y: {label: "Insert widget inside"}}
            design_up := Small{text: "↑" a11y: {label: "Move widget up"}}
            design_down := Small{text: "↓" a11y: {label: "Move widget down"}}
            design_out := Small{text: "←" a11y: {label: "Move widget out"}}
            design_wrap := Small{text: "Wrap"}
            design_dup := Small{text: "Dup" a11y: {label: "Duplicate widget"}}
            design_del := Small{text: "Del" a11y: {label: "Delete widget"}}
        }
    })
}

pub(in crate::tweak) fn wrap_value(vm: &mut ScriptVm) -> ScriptValue {
    let palette = PaletteList::register_widget(vm);
    let strip = strip_value(vm);
    script_eval!(vm, {
        use mod.prelude.workspace.*
        use mod.widgets.*
        let PaletteList = #(palette)
        let Strip = #(strip)
        let Small = PanelButton{width: Fit height: 20 min_height: 0 padding: Inset{left: 5 right: 5}
            draw_text.text_style.font_size: 8}
        View{visible: false width: Fill height: Fill{weight: 75.0} flow: Down spacing: 3
            build_status := BodyNote{width: Fill flow: Right{wrap: true}}
            board_widget := DropDown{visible: false width: Fill}
            build_strip := Strip{}
            View{width: Fill height: Fit flow: Right{wrap: true} spacing: 3
                insert_before := Small{text: "Insert before"}
                insert_inside := Small{text: "Inside"}
                insert_after := Small{text: "After"}
            }
            design_session_actions := View{width: Fill height: Fit flow: Right{wrap: true} spacing: 3
                design_undo := Small{text: "Undo"}
                design_redo := Small{text: "Redo"}
                design_reset := Small{text: "Reset"}
                design_bake := Small{text: "Bake"}
                design_diff := Small{text: "Diff"}
                design_patch := Small{text: "Patch"}
                design_commit := Small{text: "Commit" visible: false}
                design_verify := Small{text: "Verify"}
            }
            build_preview := BodyNote{visible: false width: Fill max_lines: 3 flow: Right{wrap: true}}
            palette_filter := FabSearch{input +: {empty_text: "Search widgets"}}
            build_palette := PaletteList{width: Fill height: Fill drag_chip: DragChip{}
                palette_rows := PortalList{width: Fill height: Fill reuse_items: true
                    Group := Kicker{width: Fill}
                    Entry := Small{width: Fill align: Align{x: 0.0 y: 0.5}}
                }
            }
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::tweak) enum PaletteRow { Group(String), Entry(String) }

fn matches(text: &str, query: &str) -> bool {
    let text = text.to_lowercase();
    let mut letters = text.chars();
    query.chars().all(|c| letters.any(|letter| letter == c))
}

#[derive(Default)]
pub struct Palette {
    groups: BTreeMap<String, Vec<String>>,
    query: String,
    pub(in crate::tweak) rows: Vec<PaletteRow>,
}

impl Palette {
    pub(in crate::tweak) fn page(&mut self, json: &Value, first: bool) -> Result<(), String> {
        let rows = json.get("palette").and_then(Value::as_arr).ok_or("Invalid design palette")?;
        if first { self.groups.clear(); }
        for row in rows {
            let name = row.get("name").and_then(Value::as_str).ok_or("Missing widget name")?;
            let group = row.get("group").and_then(Value::as_str).ok_or("Missing widget group")?;
            self.groups.entry(group.to_owned()).or_default().push(name.to_owned());
        }
        self.filter(self.query.clone());
        Ok(())
    }

    pub(in crate::tweak) fn filter(&mut self, query: String) {
        self.query = query.trim().to_lowercase();
        self.rows.clear();
        for (group, entries) in &self.groups {
            let group_matches = matches(group, &self.query);
            let mut items = entries.iter().filter(|name| group_matches || matches(name, &self.query)).peekable();
            if items.peek().is_some() {
                self.rows.push(PaletteRow::Group(group.clone()));
                self.rows.extend(items.cloned().map(PaletteRow::Entry));
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct PaletteList {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] pub palette: Palette,
    #[live] drag_chip: ScriptObjectRef,
    #[rust] drag_enabled: bool,
    #[rust] press: Option<(DVec2, String)>,
    #[rust] dragged: bool,
}

impl PaletteList {
    pub(in crate::tweak) fn set_drag_enabled(&mut self, enabled: bool) {
        self.drag_enabled = enabled;
        if !enabled { self.press = None; }
    }
}

impl Widget for PaletteList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event {
            Event::MouseDown(e) if e.button.is_primary() => {
                self.dragged = false;
                self.press = self.drag_enabled.then(|| {
                    let i = self.view.portal_list(cx, ids!(palette_rows)).item_at(cx, e.abs)?;
                    match self.palette.rows.get(i)? { PaletteRow::Entry(ty) => Some((e.abs, ty.clone())), _ => None }
                }).flatten();
            }
            Event::MouseMove(e) if self.drag_enabled && self.press.as_ref().is_some_and(|(at, _)| (e.abs - *at).length() > 4.) => {
                let (_, ty) = self.press.take().unwrap();
                if cx.has_internal_drag() { return }
                let chip = cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.drag_chip.as_object().into()));
                chip.label(cx, ids!(label)).set_text(cx, &ty);
                let item = DragItem::String { value: ty, internal_id: Some(live_id!(town_design_palette)) };
                cx.start_dragging_with(vec![item], chip, dvec2(14., 10.));
                self.dragged = true;
            }
            Event::MouseUp(_) | Event::DragEnd => self.press = None,
            Event::WindowLostFocus(_) => { self.press = None; self.dragged = false; }
            _ => {}
        }
        if self.dragged && matches!(event, Event::MouseUp(_)) {
            let mut actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
            actions.retain(|a| !a.as_widget_action().is_some_and(|w| matches!(w.action.downcast_ref::<ButtonAction>(), Some(ButtonAction::Clicked(_)))));
            cx.extend_actions(actions);
            self.dragged = false;
        } else { self.view.handle_event(cx, event, scope); }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let Some(mut list) = step.borrow_mut::<PortalList>() else { return Err(step.clone()) };
            list.set_item_range(cx, 0, self.palette.rows.len());
            while let Some(i) = list.next_visible_item(cx) {
                let Some(row) = self.palette.rows.get(i) else { continue };
                let (template, text) = match row {
                    PaletteRow::Group(s) => (id!(Group), s),
                    PaletteRow::Entry(s) => (id!(Entry), s),
                };
                let widget = list.item(cx, i, template);
                if widget.text() != *text { widget.set_text(cx, text); }
                widget.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}

#[derive(Default)]
pub(in crate::tweak) struct BuildTab {
    ty: String,
    pub(in crate::tweak) loaded: bool,
    pub(in crate::tweak) pending: BTreeMap<super::super::RequestId, DesignRequest>,
}

impl BuildTab {
    pub(in crate::tweak) fn ty(&self) -> &str { if self.ty.is_empty() { "View" } else { &self.ty } }

    pub(in crate::tweak) fn palette_action(&mut self, cx: &mut Cx, root: &WidgetRef, actions: &Actions) -> bool {
        let search = root.text_input(cx, ids!(palette_filter.input));
        let rows = root.portal_list(cx, ids!(palette_rows));
        let widget = root.widget(cx, ids!(build_palette));
        let Some(mut list) = widget.borrow_mut::<PaletteList>() else { return false };
        let query = if search.escaped(actions) { search.set_text(cx, ""); Some(String::new()) } else { search.changed(actions) };
        if let Some(query) = query {
            list.palette.filter(query);
            if let Some(mut rows) = rows.borrow_mut() { rows.set_first_id_and_scroll(0, 0.); }
            list.redraw(cx);
            return true;
        }
        for (index, item) in rows.items_with_actions(actions) {
            if let Some(PaletteRow::Entry(ty)) = list.palette.rows.get(index) {
                if item.as_button().clicked(actions) && item.text() == *ty {
                    self.ty.clone_from(ty);
                    return true;
                }
            }
        }
        false
    }

    pub(in crate::tweak) fn request(&self, cx: &Cx, root: &WidgetRef, strip: &WidgetRef,
        actions: &Actions, path: &str, build: bool) -> Option<DesignRequest> {
        let clicked = |parent: &WidgetRef, id| parent.button(cx, id).clicked(actions);
        let insert = |place| DesignRequest::Insert { path: path.to_owned(), place, ty: self.ty().into(), name: None, props: vec![] };
        if !path.is_empty() {
            for (id, dir) in [(ids!(design_up), StepDir::Up), (ids!(design_down), StepDir::Down), (ids!(design_out), StepDir::Out)] {
                if clicked(strip, id) { return Some(DesignRequest::Step { path: path.to_owned(), dir }); }
            }
            if clicked(strip, ids!(design_add)) { return Some(insert(DesignPlace::Inside)); }
            if clicked(strip, ids!(design_wrap)) { return Some(DesignRequest::Wrap { path: path.to_owned(), ty: "View".into(), name: None }); }
            if clicked(strip, ids!(design_dup)) { return Some(DesignRequest::Duplicate { path: path.to_owned(), name: None }); }
            if clicked(strip, ids!(design_del)) { return Some(DesignRequest::Delete { path: path.to_owned(), force_rust_refs: false }); }
            if build {
                for (id, place) in [(ids!(insert_before), DesignPlace::Before), (ids!(insert_inside), DesignPlace::Inside), (ids!(insert_after), DesignPlace::After)] {
                    if clicked(root, id) { return Some(insert(place)); }
                }
            }
        }
        if !build { return None }
        for (id, req) in [
            (ids!(design_undo), DesignRequest::Undo), (ids!(design_redo), DesignRequest::Redo),
            (ids!(design_reset), DesignRequest::Reset),
            (ids!(design_bake), DesignRequest::Bake { scope: BakeScope::All, path: None, prop: None }),
            (ids!(design_diff), DesignRequest::Diff { file: None, offset: 0, limit: super::super::wire::MAX_DIFF_LIMIT }),
            (ids!(design_patch), DesignRequest::WritePatch { file: None }),

        ] {
            if clicked(root, id) { return Some(req); }
        }
        clicked(root, ids!(design_verify)).then(|| DesignRequest::Verify { path: (!path.is_empty()).then(|| path.to_owned()) })
    }
}

pub(in crate::tweak) fn enable_selection(cx: &mut Cx, root: &WidgetRef, enabled: bool) {
    for id in [ids!(build_strip), ids!(tree_strip)] {
        let strip = root.widget(cx, id);
        for id in [ids!(design_add), ids!(design_up), ids!(design_down), ids!(design_out), ids!(design_wrap), ids!(design_dup), ids!(design_del)] {
            strip.button(cx, id).set_enabled(cx, enabled);
        }
    }
    for id in [ids!(insert_before), ids!(insert_inside), ids!(insert_after)] {
        root.button(cx, id).set_enabled(cx, enabled);
    }
}
