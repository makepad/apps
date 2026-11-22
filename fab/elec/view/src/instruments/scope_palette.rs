//! The measure catalog and its retained, snapshot-keyed overlay.

use super::*;

const TIMING_ANSWERS: &[&str] = &["freq", "duty", "width", "rise"];
const AMPLITUDE_ANSWERS: &[&str] = &["Vpp", "mean", "top", "base", "min", "max", "overshoot"];
pub(super) const SPECTRUM_ANSWERS: &[&str] = &["peak f", "peak", "2nd f", "2nd", "H2", "H3", "H4", "H5", "RBW", "span"];
const GROUPS: &[(&str, &[&str])] = &[("timing", TIMING_ANSWERS), ("amplitude", AMPLITUDE_ANSWERS), ("spectrum", SPECTRUM_ANSWERS)];

pub(super) fn answer_name(name: &str) -> Option<&'static str> {
    GROUPS.iter().flat_map(|(_, names)| names.iter().copied()).find(|candidate| *candidate == name)
}

impl SignalLane {
    fn measure_popover_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        if self.measure_popover_widget.is_none() {
            self.measure_popover_widget = Some(cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.measure_popover.as_object().into())));
            cx.widget_tree_mark_dirty(self.uid);
        }
        self.measure_popover_widget.as_ref().unwrap().clone()
    }

    fn update_measure_palette(&mut self, cx: &mut Cx, popover: &PopoverRef) {
        let channel = self.settings.channels.first().map_or(0, |c| c.color_index);
        let key = (self.measure_revision, self.dark, channel);
        if self.measure_popover_key == Some(key) { return; }
        let mut entries = vec![("automatic", false)];
        for &(group, names) in GROUPS {
            if group == "spectrum" && self.spectrum().is_none() { continue; }
            entries.push((group, true));
            entries.extend(names.iter().map(|&name| (name, false)));
        }
        let content = popover.content();
        let uid = content.widget_uid();
        let rows = if let Some(mut view) = content.borrow_mut::<View>() {
            let changed = view.children.len() != entries.len();
            while view.children.len() < entries.len() {
                let id = LiveId::from_str(entries[view.children.len()].0);
                let row = cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.measure_row.as_object().into()));
                cx.widget_tree_insert_child(uid, id, row.clone());
                view.children.push((id, row));
            }
            view.children.truncate(entries.len());
            if changed { cx.widget_tree_mark_dirty(uid); }
            view.children.iter().map(|(_, row)| row.clone()).collect::<Vec<_>>()
        } else { return };
        // A collapsed meter still edits the choices retained for its expanded lane.
        let selected = self.selected_answer_names(false);
        let ripple = selected.first() == Some(&"mean");
        let look = &InstrumentLook::for_theme(self.dark).on_panel();
        for ((name, heading), row) in entries.into_iter().zip(rows) {
            let label = row.label(cx, ids!(heading));
            label.set_visible(cx, heading);
            let choice = row.button(cx, ids!(choice));
            choice.set_visible(cx, !heading);
            if heading {
                label.set_text(cx, name);
                let ink = look.text_tertiary;
                label.set_text_color(cx, vec4(ink[0], ink[1], ink[2], ink[3]));
                continue;
            }
            let pinned = self.pins.iter().any(|q| q.name() == name);
            let required = self.answer_required(name);
            let checked = if name == "automatic" { self.settings.answers.is_none() } else { selected.contains(&name) };
            let check = if checked { "✓" } else { " " };
            let text = if name == "automatic" { format!("{check} automatic") } else {
                let answers = self.named_answers(name, ripple);
                let value = answers.items.first().map_or_else(|| "-".into(), |answer| {
                    if answer.unit.is_empty() { answer.value.clone() } else { format!("{}\u{2009}{}", answer.value, answer.unit) }
                });
                format!("{check} {name}   {value}{}", if pinned { "   pinned" } else if required { "   expected" } else { "" })
            };
            choice.set_text(cx, &text);
            choice.set_enabled(cx, !required);
            if let Some(mut button) = choice.borrow_mut() {
                let ink = if required { look.text_tertiary } else { look.text };
                button.draw_text.color = vec4(ink[0], ink[1], ink[2], ink[3]);
            };
        }
        self.measure_popover_key = Some(key);
        content.redraw(cx);
    }

    pub(super) fn toggle_measure_palette(&mut self, cx: &mut Cx) {
        let Some(rect) = self.answer_part_rect(Part::AddAnswer) else { return };
        let popover = self.measure_popover_widget(cx).as_popover();
        if popover.is_open() { popover.close(cx); self.measure_anchor = None; }
        else {
            self.measure_popover_key = None;
            self.update_measure_palette(cx, &popover);
            let rect = Rect { pos: rect.pos + self.area.rect(cx).pos - self.drawn_at, ..rect };
            cx.set_key_focus_from_user(self.area);
            popover.open_at(cx, rect);
            self.measure_anchor = Some(rect);
            self.focused = Some(Part::AddAnswer);
        }
        self.area.redraw(cx);
    }

    fn measure_palette_actions(&mut self, cx: &mut Cx, popover: &PopoverRef, actions: &Actions) {
        if !popover.is_open() || actions.is_empty() { return; }
        let content = popover.content();
        let mut edits = Vec::new();
        if content.button(cx, ids!(automatic.choice)).clicked(actions) { edits = self.settings.set_automatic_answers(); }
        for &(group, names) in GROUPS {
            if group == "spectrum" && self.spectrum().is_none() { continue; }
            for &name in names {
                if !self.answer_required(name) && content.button(cx, &[LiveId::from_str(name), id!(choice)]).clicked(actions) {
                    let automatic = self.automatic_answer_names(false);
                    edits = self.settings.toggle_answer(name, &automatic);
                }
            }
        }
        if edits.is_empty() { return; }
        self.answers_dirty = true;
        self.answers_at = None;
        self.answers_layout_key = None;
        self.emit(cx, edits);
        self.refresh_answers(cx);
        self.update_measure_palette(cx, popover);
    }

    pub(super) fn handle_measure_palette(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        let Some(widget) = self.measure_popover_widget.clone() else { return false };
        let popover = widget.as_popover();
        let was_open = popover.is_open();
        if was_open {
            let anchor = self.answer_part_rect(Part::AddAnswer);
            if anchor != self.measure_anchor {
                if let Some(rect) = anchor { popover.open_at(cx, rect); }
                else { popover.close(cx); }
                self.measure_anchor = anchor;
            }
        }
        let actions = cx.capture_actions(|cx| widget.handle_event(cx, event, scope));
        if was_open {
            self.measure_palette_actions(cx, &popover, &actions);
            if let Event::Actions(actions) = event { self.measure_palette_actions(cx, &popover, actions); }
            if !popover.is_open() { self.measure_anchor = None; self.pointer = None; self.area.redraw(cx); }
        }
        // Escape and the outside closing press belong to this overlay, including on dismissal.
        was_open
    }

    pub(super) fn draw_measure_palette(&mut self, cx: &mut Cx2d, scope: &mut Scope) {
        // The palette widget exists only once it has been opened.
        let Some(widget) = self.measure_popover_widget.clone() else { return };
        let popover = widget.as_popover();
        let anchor = self.answer_part_rect(Part::AddAnswer);
        if anchor.is_none() { popover.close(cx); self.measure_anchor = None; }
        if popover.is_open() {
            self.update_measure_palette(cx, &popover);
            // Keep the full catalog reachable on short windows without growing the lane.
            let padding = popover.borrow().map_or(0.0, |popover| popover.panel_padding.top + popover.panel_padding.bottom);
            let room = (cx.current_pass_size().y - padding - 2.0 * ROW).max(0.0);
            let content = popover.content();
            if let Some(mut view) = content.borrow_mut::<View>() {
                view.walk.height = Size::Fixed((view.children.len() as f64 * 26.0).min(room));
            };
        }
        widget.draw_walk_all(cx, scope, Walk::abs_rect(anchor.unwrap_or_default()));
    }
}
