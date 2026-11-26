//! The inspector card's half of the explorer (user, 11-18: "clicking something and changing the
//! value of a resistor"): the selection a click makes in place ([`ExplorerView::select_part`]: no
//! layout, no camera move), its card ([`super::super::inspector`]), and its reference and value
//! fields. A reference writes `CircuitOp::SetRef`. A value typed or scrubbed is
//! `CircuitOp::SetValue`, written back to the source and heard at once by a playing run.
//!
//! Cost: per draw while a part is selected O(1) quads and text runs (the card's text is cached per
//! part and design); per selection one pass over the part's connections; nothing otherwise.

use super::*;
use fab_elec_lang::{map::site_text, units::parse_value};
use crate::explorer::inspector::{card_height, card_text, place_card, CardText, CARD_PAD, CARD_WIDTH, CHIP_ROW, ROW, VALUE_ROW};
use crate::explorer::body_source::{body_plan, BodyPick};
use fab_elec_parts::model_board::{self, Choice, Refused};
use crate::explorer::value_edit::{edit_text, value_presets, ReferenceEdit};
use fab_elec_circuit::{Component, RefKind};
use fab_elec_lang::source::LIB_DOC;
use std::borrow::Cow;

/// The inspector card as last drawn: its part, the design its text was made from (by pointer),
/// that text, its field hit areas, and the latest source edit's refusal.
pub(super) struct Card {
    node: u32,
    design: usize,
    /// The model board's change count the text was made at.
    models: u64,
    text: CardText,
    reference_editable: bool,
    reference_width: f64,
    reference_area: Area,
    value_area: Area,
    /// The 3D model rows' licences link, the body switch (one area per [`BodyPick::ALL`]) and this
    /// computer's switch (one per [`Choice::ALL`]).
    licences_area: Area,
    body_areas: [Area; 3],
    from_areas: [Area; 3],
    /// The model's detail as it fits the card (a path keeps its end), measured once per text.
    detail: String,
    sent: Option<(String, u64)>,
    note: Option<String>,
}

/// The person asks for the External Resources and Licences page from a part's 3D model rows: the
/// shell opens it.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowModelSources;

/// The switch's words and widths, in [`Choice::ALL`]'s order.
const FROM_CHIPS: [(&str, f64); 3] = [("auto", 46.0), ("this computer", 92.0), ("download", 68.0)];

/// What the card does with a click in its 3D model rows.
#[derive(Clone, Copy)]
enum ModelClick {
    Licences,
    Body(BodyPick),
    From(Choice),
}

/// A refused choice in the card's few words.
fn refused_text(refused: &Refused) -> String {
    match refused {
        Refused::SourceOff { .. } => "that source is off in 3D Model Sources".into(),
        Refused::NotOffered { .. } | Refused::NotAModel(_) => "no KiCad library model to choose for".into(),
    }
}

/// Whole-row screen text in the card, cut to its width.
/// `text` as it fits `width` in `draw_text`'s style, cut from the front (`…/kicad3d/3dmodels`):
/// a folder or a download keeps its end. O(log length) layouts.
fn fit_tail(draw_text: &mut DrawText, cx: &mut Cx2d, text: &str, width: f64) -> String {
    let mut fits = |text: &str| draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), text).size_in_lpxs.width as f64 <= width;
    if fits(text) { return text.to_string(); }
    let starts: Vec<usize> = text.char_indices().map(|(at, _)| at).skip(1).collect();
    let tail = |dropped: usize| format!("…{}", starts.get(dropped).map_or("", |&at| &text[at..]));
    // the fewest characters dropped from the front for the rest to fit after the ellipsis
    let (mut lo, mut hi) = (0, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(&tail(mid)) { hi = mid } else { lo = mid + 1 }
    }
    tail(lo)
}

fn clip(text: &str, chars: usize) -> Cow<'_, str> {
    if text.chars().nth(chars).is_none() { return Cow::Borrowed(text); }
    let mut cut: String = text.chars().take(chars.saturating_sub(1)).collect();
    cut.push('…');
    Cow::Owned(cut)
}

/// Text typed over the sheet that starts a new value: a digit or a decimal point first.
pub(super) fn starts_value(text: &str) -> bool {
    text.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.')
}

fn rgba(c: [f32; 4]) -> Vec4f { vec4(c[0], c[1], c[2], c[3]) }

fn mix_rgba(a: [f32; 4], b: [f32; 4], t: f32, alpha: f32) -> Vec4f {
    vec4(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, alpha)
}

/// Automatic references carry the call's ownership from evaluation. An explicit reference
/// can also be shared, so check the items at its mapped call when making or opening the field.
fn reference_editable(cx: &Cx, part: CompRef, kind: RefKind) -> bool {
    match kind {
        RefKind::Auto => true,
        RefKind::Shared => false,
        RefKind::Stored => {
            let Some(map) = cx.get_global_ref::<ElecLive>().and_then(|live| live.snap.source_map.as_ref()) else { return false };
            map.locate(&Sel::Comp(part)).is_some_and(|at| at.doc.0 & LIB_DOC == 0
                && !map.all_at(at.doc, at.span.start).iter().any(|other|
                    matches!(other, Sel::Comp(c) if *c != part) && map.locate(other).as_ref() == Some(&at)))
        }
    }
}

pub(super) fn reference_field(board: BoardId, comp: &Component) -> ReferenceEdit {
    ReferenceEdit {
        part: CompRef { board, comp: comp.id }, path: comp.path.clone(),
        reference: comp.refdes.clone(), kind: comp.ref_kind,
    }
}

impl ExplorerView {
    /// The first selected part in pick order. O(1).
    pub(super) fn selected_node(&self) -> Option<u32> {
        self.selected.first().map(|(node, _)| *node)
    }

    /// Replace the local selection and close the old card. O(selected).
    pub(super) fn set_selection(&mut self, cx: &mut Cx, selected: Vec<(u32, String)>) -> bool {
        if self.selected == selected { return false; }
        self.selected = selected;
        self.card = None;
        if self.card_press.take().is_some_and(|(_, scrubbing)| scrubbing) {
            self.end_scrub(cx, false);
        }
        if self.value_edit.is_some() || matches!(self.name_edit, Some(NameEdit::Reference(_))) { self.close_value_edit(cx); }
        self.draw_bg.redraw(cx);
        true
    }

    /// Publish the committed selection. During a band a source edit can rebind the selection
    /// from its press, while the preview stays local. O(selected).
    pub(super) fn publish_selection(&self, cx: &mut Cx) {
        let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) else { return };
        let sel = |node| Sel::Comp(CompRef { board: unit.id, comp: CompId(node) });
        let parts = match &self.marquee {
            Marquee::Drawing { before, .. } => before,
            _ => &self.selected,
        };
        match parts.as_slice() {
            [] => cx.widget_action(self.uid, ViewAction::Unfocus),
            [(node, _)] => cx.widget_action(self.uid, ViewAction::Focus(Focus {
                kind: FocusKind::Part, sel: sel(*node), origin: FocusOrigin::Explorer,
            })),
            parts => cx.widget_action(self.uid, crate::ViewState {
                selected: parts.iter().map(|(node, _)| sel(*node)).collect(), emphasis: Vec::new(),
            }),
        }
    }

    /// Rebind paths once when the source changes. O(selected), plus O(parts) if indices moved.
    pub(super) fn rebind_selection(&mut self, cx: &mut Cx) {
        let band_empty = match &self.marquee {
            Marquee::Drawing { before, .. } => before.is_empty(),
            _ => true,
        };
        if self.selected.is_empty() && band_empty { return; }
        let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) else { return };
        let mut paths = ByPath::new(&unit.circuit);
        let mut rebind = |parts: &mut Vec<(u32, String)>| {
            let mut changed = false;
            parts.retain_mut(|(node, path)| {
                let next = paths.find(*node as usize, path).map(|n| n as u32);
                changed |= next != Some(*node);
                if let Some(next) = next { *node = next; }
                next.is_some()
            });
            changed
        };
        let changed = rebind(&mut self.selected);
        if changed {
            self.card = None;
            self.card_press = None;
        }
        let committed_changed = match &mut self.marquee {
            Marquee::Drawing { before, .. } => rebind(before),
            _ => changed,
        };
        if committed_changed { self.publish_selection(cx); }
    }

    /// Commit one sheet pick. O(selected).
    pub(super) fn select_parts(&mut self, cx: &mut Cx, selected: Vec<(u32, String)>) {
        self.end_marquee(cx);
        if self.set_selection(cx, selected) { self.publish_selection(cx); }
    }

    /// Select a part in place, or toggle its membership on a Shift-click. O(selected).
    pub(super) fn select_part(&mut self, cx: &mut Cx, node: u32, toggle: bool) {
        let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) else { return };
        let Some(comp) = unit.circuit.comps.get(node as usize) else { return };
        let mut parts = if toggle { self.selected.clone() } else { Vec::new() };
        if let Some(index) = parts.iter().position(|(n, _)| *n == node) {
            parts.remove(index);
        } else {
            parts.push((node, comp.path.clone()));
        }
        self.select_parts(cx, parts);
    }

    /// Clear the selection on a plain click on empty sheet or Escape.
    pub(super) fn clear_selection(&mut self, cx: &mut Cx) {
        self.select_parts(cx, Vec::new());
    }

    /// Follow another view's selection without publishing it again. O(actions + selected).
    pub(super) fn take_focus(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions.iter().rev() {
            let widget = action.as_widget_action();
            let selected = if let Some(view) = widget.and_then(|w| w.action.downcast_ref::<crate::ViewState>()) {
                view.selected.as_slice()
            } else {
                match widget.and_then(|w| w.action.downcast_ref::<ViewAction>()).or_else(|| action.downcast_ref::<ViewAction>()) {
                    Some(ViewAction::Focus(focus)) => std::slice::from_ref(&focus.sel),
                    Some(ViewAction::Unfocus) => &[],
                    _ => continue,
                }
            };
            if widget.is_some_and(|w| w.widget_uid == self.uid) { return; }
            let unit = self.design.as_ref().and_then(|d| d.units.get(self.unit));
            let parts = selected.iter().filter_map(|sel| {
                let Sel::Comp(comp) = sel else { return None };
                let part = unit.filter(|unit| unit.id == comp.board)?.circuit.comps.get(comp.comp.ix())?;
                Some((comp.comp.0, part.path.clone()))
            }).collect();
            self.end_marquee(cx);
            self.set_selection(cx, parts);
            return;
        }
    }

    /// Resolve the drawn card's part in the current source and the visible sheet in O(1) lookups.
    fn card_part(&self) -> Option<(BoardId, &Component)> {
        let node = self.selected_node()?;
        let design = self.design.as_ref()?;
        self.card.as_ref().filter(|card| card.node == node)?;
        let unit = design.units.get(self.unit)?;
        let comp = unit.circuit.comps.get(node as usize)?;
        let shown = self.sheet_design.as_ref()?.units.get(self.unit)?;
        (shown.id == unit.id && shown.circuit.comps.get(node as usize)?.path == comp.path).then_some((unit.id, comp))
    }

    /// Select the displayed reference after checking the source part still owns the call.
    fn open_reference_field(&mut self, cx: &mut Cx) {
        if self.field_open() || self.busy() || self.scrub.is_some() { return; }
        let Some((board, comp)) = self.card_part() else { return };
        let part = CompRef { board, comp: comp.id };
        if !self.card.as_ref().is_some_and(|card| card.reference_editable && card.text.reference == comp.refdes)
            || !reference_editable(cx, part, comp.ref_kind) { return; }
        let edit = reference_field(board, comp);
        self.focus_name_field(cx, &edit.reference);
        self.name_edit = Some(NameEdit::Reference(edit));
    }

    /// Open the card's value field on the selected part: its value selected whole, or `typed`
    /// (a key pressed over the sheet) in its place. The commit is `CircuitOp::SetValue`, written
    /// back to its source; a value the source cannot take here says why in the card.
    pub(super) fn open_card_field(&mut self, cx: &mut Cx, typed: Option<TextInputEvent>) {
        if self.field_open() || self.busy() || self.scrub.is_some() { return; }
        if !self.card.as_ref().is_some_and(|card| card.text.value.is_some()) { return; }
        let Some((board, comp)) = self.card_part() else { return };
        let Some(current) = comp.value.as_ref() else { return };
        let site = if cx.has_global::<ElecLive>() {
            let snap = &cx.get_global::<ElecLive>().snap;
            site_text(snap.source_map.as_ref().and_then(|map| map.value_site(&CompRef { board, comp: comp.id })), &comp.path)
        } else { site_text(None, &comp.path) };
        let site = match site {
            Ok(site) => site,
            Err(error) => {
                if let Some(card) = &mut self.card { card.note = Some(error); }
                self.draw_bg.redraw(cx);
                return;
            }
        };
        let combo = self.value_combo.as_combo_box();
        let presets = value_presets(current.qty.map(|q| q.unit));
        combo.set_labels(cx, presets.iter().map(|&(label, _)| label.to_owned()).collect());
        combo.set_text(cx, edit_text(current));
        self.value_edit = Some(ValueEdit {
            comp: comp.id, unit: board, refdes: comp.refdes.clone(), current: current.clone(),
            site, kind: comp.part.kind.clone(), pending_keys: Some(typed.map(Event::TextInput).into_iter().collect()),
        });
        self.value_focus_pending = true;
        self.pane.nav.stop();
        self.draw_bg.redraw(cx);
    }

    /// Route the card field before the sheet. Keys that arrived before its first draw are
    /// delivered to the focused input in order, including the digit that opened the editor.
    pub(super) fn value_combo_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        if self.value_edit.is_none() && !matches!(event, Event::KeyFocus(_)) { return false; }
        let combo = self.value_combo.as_combo_box();
        let input = combo.input_area();
        let lost = matches!(event, Event::KeyFocus(e) if !input.is_empty() && e.prev == input);
        let Some(edit) = &mut self.value_edit else {
            // The hidden input still needs its final focus loss to stop the caret timer.
            if lost { self.value_combo.handle_event(cx, event, scope); }
            return false;
        };
        let focused = !input.is_empty() && cx.has_key_focus(input);
        if let Some(keys) = &mut edit.pending_keys {
            if self.value_focus_pending || !focused {
                return queue_field_key(keys, event);
            }
            let keys = edit.pending_keys.take().unwrap_or_default();
            let gained = matches!(event, Event::KeyFocus(e) if e.focus == input);
            if gained { self.value_combo.handle_event(cx, event, scope); }
            for key in keys {
                if self.value_edit.is_some() {
                    self.dispatch_value_combo(cx, &key, scope);
                } else if matches!(self.name_edit, Some(NameEdit::Reference(_))) {
                    self.name_keys.push(key);
                } else { break; }
            }
            if gained { return false; }
            if self.value_edit.is_none() { return self.name_edit.is_none(); }
        }
        self.dispatch_value_combo(cx, event, scope)
    }

    /// Apply one field event. Picks and typed submissions both go through the source value parser.
    fn dispatch_value_combo(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        let Some(edit) = &self.value_edit else { return false };
        let presets = value_presets(edit.current.qty.map(|q| q.unit));
        let combo = self.value_combo.as_combo_box();
        let input = combo.input_area();
        let open = combo.is_open();
        let focused = !input.is_empty() && cx.has_key_focus(input);
        let actions = cx.capture_actions(|cx| self.value_combo.handle_event(cx, event, scope));
        let tab = focused && matches!(event, Event::KeyDown(key) if key.key_code == KeyCode::Tab);
        // The popup owns an outside press. If it dismissed the list over Reference, that one
        // press also opens the field. A press inside the list still belongs to its row.
        let reference_press = open && !combo.is_open() && self.card.as_ref().is_some_and(|card| {
            if !card.reference_editable { return false; }
            let rect = card.reference_area.clipped_rect(cx);
            match event {
                Event::MouseDown(e) => e.button.is_primary() && rect.contains(e.abs),
                Event::TouchUpdate(e) => e.touches.iter().any(|t| t.state == TouchState::Start && rect.contains(t.abs)),
                _ => false,
            }
        });
        if presets.is_empty() && combo.is_open() { combo.close_list(cx); }
        if let Some(&(label, _)) = combo.selected(&actions).and_then(|i| presets.get(i)) {
            self.commit_value_text(cx, label);
        } else if let Some(text) = combo.typed(&actions) {
            self.commit_value_text(cx, &text);
        } else if reference_press || matches!(event, Event::KeyFocus(e) if !input.is_empty() && e.prev == input)
            || matches!(event, Event::KeyDown(key) if focused && matches!(key.key_code,
                KeyCode::Escape | KeyCode::ReturnKey | KeyCode::NumpadEnter | KeyCode::Tab))
        {
            // The combo restores a blank submission or Escape without publishing a value.
            self.close_value_edit(cx);
        }
        if (tab || reference_press) && self.value_edit.is_none() { self.open_reference_field(cx); }
        (focused && matches!(event, Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)))
            || (open && matches!(event, Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseMove(_)
                | Event::TouchUpdate(_) | Event::Scroll(_)))
    }

    /// A drag along the card's value from `start` (absolute) scrubs it through its preferred
    /// numbers, as a drag on its label on the sheet does (`start_scrub`): previews while it moves,
    /// one commit on release. False when the value has no series.
    pub(super) fn start_card_scrub(&mut self, cx: &mut Cx, start: DVec2) -> bool {
        if self.field_open() || self.busy() || self.scrub.is_some() { return false; }
        let Some(node) = self.selected_node() else { return false };
        let Some(unit) = self.design.as_ref().and_then(|design| design.units.get(self.unit)) else { return false };
        let Some(comp) = unit.circuit.comps.get(node as usize) else { return false };
        let Some(value) = comp.value.as_ref() else { return false };
        let Some(scrub) = Scrub::begin(comp.id, value) else { return false };
        let now = cx.seconds_since_app_start();
        // The value's label on the sheet follows the scrub where the sheet shows one.
        let text = match self.scene.value_text(node).cloned() {
            Some(mut text) => {
                let offset = self.scene.offset(node, now);
                text.at = [text.at[0] + offset[0], text.at[1] + offset[1]];
                text.text = scrub.value().text;
                text.fade = TextFade::REST;
                text
            }
            None => Text {
                text: String::new(), at: [0.0, 0.0], size: sheet::TEXT_VALUE, scale: 1.0, hidden: true,
                align: glyphs::Align::Left, vertical: false, color: Tokens::for_theme(self.dark).ink,
                item: NO_ITEM, owner: None, is_value: false, fade: TextFade::REST,
            },
        };
        let from = self.pane.cam.to_world(start);
        self.scrub = Some(ValueScrub { scrub, unit: unit.id, from: [from[0], from[1]], text, previewed: false });
        self.rebuild_chip(Tokens::for_theme(self.dark));
        self.draw_bg.redraw(cx);
        true
    }

    /// The card's pointer input, before the flags and the sheet: its value takes a click (type a
    /// new one) or a drag (scrub it); the rest of the card only keeps the pointer off the sheet.
    /// True when the event was the card's.
    pub(super) fn card_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Some(card) = &self.card else { return false };
        let scrubbable = card.text.editable;
        if card.reference_editable && self.name_edit.is_none() {
            match event.hits(cx, card.reference_area) {
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                    cx.set_cursor(MouseCursor::Text);
                    let now = cx.seconds_since_app_start();
                    if let Some(input) = self.pane.clear_hover(now) { self.input(cx, input); }
                    return true;
                }
                Hit::FingerDown(e) if e.is_primary_hit() => {
                    if self.value_edit.is_some() { cx.set_key_focus(self.draw_bg.area()); }
                    return true;
                }
                Hit::FingerUp(e) if e.is_primary_hit() => {
                    if e.is_over { self.open_reference_field(cx); }
                    return true;
                }
                _ => {}
            }
        }
        if card.text.model.is_some() {
            let areas = std::iter::once((card.licences_area, ModelClick::Licences))
                .chain(card.body_areas.into_iter().zip(BodyPick::ALL.map(ModelClick::Body)))
                .chain(card.from_areas.into_iter().zip(Choice::ALL.map(ModelClick::From)));
            let mut pick = None;
            for (area, choice) in areas {
                match event.hits(cx, area) {
                    Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                        cx.set_cursor(MouseCursor::Hand);
                        let now = cx.seconds_since_app_start();
                        if let Some(input) = self.pane.clear_hover(now) { self.input(cx, input); }
                        return true;
                    }
                    Hit::FingerDown(e) if e.is_primary_hit() => return true,
                    Hit::FingerUp(e) if e.is_primary_hit() => {
                        if e.is_over { pick = Some(choice); }
                        break;
                    }
                    _ => {}
                }
            }
            match pick {
                Some(ModelClick::Licences) => {
                    cx.action(ShowModelSources);
                    return true;
                }
                Some(ModelClick::Body(pick)) => {
                    self.pick_body(cx, pick);
                    return true;
                }
                Some(ModelClick::From(choice)) => {
                    self.choose_model(cx, choice);
                    return true;
                }
                None => {}
            }
        }
        let Some(card) = &self.card else { return false };
        if self.value_edit.is_none() && card.text.value.is_some() {
            match event.hits(cx, card.value_area) {
                Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                    cx.set_cursor(if scrubbable { MouseCursor::EwResize } else { MouseCursor::Text });
                    let now = cx.seconds_since_app_start();
                    if let Some(input) = self.pane.clear_hover(now) { self.input(cx, input); }
                    return true;
                }
                Hit::FingerDown(e) if e.is_primary_hit() => {
                    self.card_press = Some((e.abs, false));
                    return true;
                }
                Hit::FingerMove(e) => {
                    if let Some((start, scrubbing)) = self.card_press {
                        if scrubbable && !scrubbing && (e.abs - start).length() > crate::canvas::pane::DRAG_THRESHOLD {
                            let started = self.start_card_scrub(cx, start);
                            self.card_press = Some((start, started));
                        }
                        if self.card_press.is_some_and(|(_, scrubbing)| scrubbing) {
                            let at = self.pane.cam.to_world(e.abs);
                            if self.drag_scrub(cx, [at[0], at[1]]) { self.draw_bg.redraw(cx); }
                        }
                    }
                    return true;
                }
                Hit::FingerUp(e) => {
                    match self.card_press.take() {
                        Some((_, true)) => self.end_scrub(cx, true),
                        Some((_, false)) if e.is_over => self.open_card_field(cx, None),
                        _ => {}
                    }
                    self.draw_bg.redraw(cx);
                    return true;
                }
                _ => {}
            }
        }
        match event.hits(cx, self.draw_card.area()) {
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                cx.set_cursor(MouseCursor::Default);
                let now = cx.seconds_since_app_start();
                if let Some(input) = self.pane.clear_hover(now) { self.input(cx, input); }
                true
            }
            Hit::FingerDown(_) | Hit::FingerMove(_) | Hit::FingerUp(_) | Hit::FingerScroll(_) | Hit::FingerLongPress(_) => true,
            _ => false,
        }
    }

    /// Draw the selected part's inspector card in a corner of the pane `pane` that leaves the part
    /// in sight: its name, its value (the field while typing, the scrub's value while dragging) and
    /// its facts. The card opens for a single selection only.
    pub(super) fn draw_inspector(&mut self, cx: &mut Cx2d, scope: &mut Scope, pane: Rect, tokens: &Tokens, now: f64) {
        let [(node, _)] = self.selected.as_slice() else {
            self.card = None;
            return;
        };
        let node = *node;
        let Some(design) = self.design.clone() else { return };
        let Some(unit) = design.units.get(self.unit) else { return };
        let Some(comp) = unit.circuit.comps.get(node as usize) else { return };
        let key = Arc::as_ptr(&design) as usize;
        let models = model_board::generation();
        if !self.card.as_ref().is_some_and(|card| card.node == node && card.design == key && card.models == models) {
            let previous = self.card.take().filter(|card| card.node == node && card.text.reference == comp.refdes);
            let (sent, note, reference_area, value_area, licences_area, body_areas, from_areas) = previous.map(|card|
                (card.sent, card.note, card.reference_area, card.value_area, card.licences_area, card.body_areas, card.from_areas)).unwrap_or_default();
            let map = cx.get_global_ref::<ElecLive>().and_then(|live| live.snap.source_map.clone());
            let site = map.as_ref().and_then(|map| map.body_site(unit.id, &comp.part.name));
            let reference_editable = reference_editable(cx, CompRef { board: unit.id, comp: comp.id }, comp.ref_kind);
            if !reference_editable && matches!(self.name_edit, Some(NameEdit::Reference(_))) { self.close_value_edit(cx); }
            self.draw_text.text_style.font_size = 11.0;
            let width = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &clip(&comp.refdes, 14)).size_in_lpxs.width as f64;
            let text = card_text(&unit.circuit, comp, site);
            self.draw_text.text_style.font_size = 9.5;
            let detail = text.model.as_ref()
                .map(|model| fit_tail(&mut self.draw_text, cx, &model.detail, CARD_WIDTH - CARD_PAD * 2.0)).unwrap_or_default();
            self.card = Some(Card {
                node, design: key, models, text, detail,
                reference_width: if reference_editable { (width + 12.0).clamp(52.0, 100.0) } else { width.min(100.0) },
                reference_editable, reference_area, value_area, licences_area, body_areas, from_areas, sent, note,
            });
        }
        // The part on screen, in the reveal while one is up, so the card leaves it in sight.
        let scene = self.reveal.as_ref().map_or(&self.scene, |reveal| &reveal.scene);
        let avoid = scene.part_box(node).map(|body| {
            let offset = scene.offset(node, now);
            self.pane.cam.rect_to_screen([body[0] + offset[0], body[1] + offset[1], body[2] + offset[0], body[3] + offset[1]])
        });
        let editing = self.value_edit.as_ref().filter(|edit| edit.comp == comp.id);
        let editing_reference = matches!(&self.name_edit, Some(NameEdit::Reference(edit)) if edit.part.comp == comp.id);
        let scrubbed = self.scrub.as_ref().filter(|active| active.scrub.comp == comp.id).map(|active| active.scrub.value().text);
        let Some(card) = &mut self.card else { return };
        let note = match editing {
            Some(edit) => edit.site.as_deref().map(|site| (site, tokens.ink_dim)),
            None => card.note.as_deref().map(|note| (note, tokens.violation)),
        };
        let size = dvec2(CARD_WIDTH, card_height(&card.text, usize::from(note.is_some())));
        let rect = place_card(pane, size, avoid);
        let width = CARD_WIDTH - CARD_PAD * 2.0;
        let reference_box = Rect { pos: rect.pos + dvec2(CARD_PAD, CARD_PAD), size: dvec2(card.reference_width, VALUE_ROW) };
        cx.push_clip_rect(pane);
        self.draw_card.color = mix_rgba(tokens.canvas, tokens.ink, 0.07, 0.97);
        self.draw_card.draw_abs(cx, rect);
        let mut y = reference_box.pos.y;
        self.draw_text.text_style.font_size = 11.0;
        self.draw_text.color = rgba(tokens.ink);
        if !editing_reference {
            let inset = if card.reference_editable {
                self.draw_card_field.color = mix_rgba(tokens.canvas, tokens.ink, 0.14, 1.0);
                self.draw_card_field.draw_abs(cx, reference_box);
                cx.add_rect_area(&mut card.reference_area, reference_box);
                6.0
            } else { 0.0 };
            cx.push_clip_rect(reference_box);
            self.draw_text.draw_abs(cx, reference_box.pos + dvec2(inset, 4.0), &clip(&card.text.reference, 14));
            cx.pop_clip_rect();
        }
        let name = reference_box.pos + dvec2(reference_box.size.x + 4.0, 4.0);
        cx.push_clip_rect(Rect { pos: name, size: dvec2(width - reference_box.size.x - 4.0, VALUE_ROW) });
        self.draw_text.draw_abs(cx, name, &clip(&card.text.part_name, 32));
        cx.pop_clip_rect();
        y += VALUE_ROW + 4.0;
        if let Some(value) = &card.text.value {
            let value_box = Rect { pos: dvec2(reference_box.pos.x, y), size: dvec2(width, VALUE_ROW) };
            if editing.is_some() {
                let walk = Walk {
                    abs_pos: Some(value_box.pos), width: Size::Fixed(value_box.size.x),
                    height: Size::Fixed(value_box.size.y), ..Walk::default()
                };
                cx.widget_tree_insert_child(self.uid, live_id!(value_combo), self.value_combo.clone());
                self.value_combo.draw_walk_all(cx, &mut Scope::empty(), walk);
                if self.value_focus_pending {
                    self.value_focus_pending = false;
                    let combo = self.value_combo.as_combo_box();
                    combo.focus_and_select(cx);
                    cx.set_key_focus_from_user(combo.input_area());
                }
            } else {
                self.draw_card_field.color = mix_rgba(tokens.canvas, tokens.ink, 0.14, 1.0);
                self.draw_card_field.draw_abs(cx, value_box);
                cx.add_rect_area(&mut card.value_area, value_box);
                self.draw_text.text_style.font_size = 12.0;
                self.draw_text.color = rgba(if scrubbed.is_some() { tokens.selection } else { tokens.ink });
                let shown = scrubbed.as_deref().unwrap_or(value);
                self.draw_text.draw_abs(cx, value_box.pos + dvec2(6.0, 4.0), &clip(shown, 22));
                self.draw_text.text_style.font_size = 9.0;
                self.draw_text.color = rgba(tokens.ink_dim);
                let hint = if card.text.editable { "type · drag ⇆" } else { "type" };
                self.draw_text.draw_abs(cx, value_box.pos + dvec2(value_box.size.x - 78.0, 6.0), hint);
            }
            y += VALUE_ROW + 6.0;
        }
        self.draw_text.text_style.font_size = 9.5;
        if let Some((line, color)) = note {
            self.draw_text.color = rgba(color);
            self.draw_text.draw_abs(cx, dvec2(rect.pos.x + CARD_PAD, y + 2.0), &clip(line, 40));
            y += ROW;
        }
        self.draw_text.color = rgba(tokens.ink_dim);
        for fact in &card.text.facts {
            self.draw_text.draw_abs(cx, dvec2(rect.pos.x + CARD_PAD, y + 2.0), &clip(fact, 40));
            y += ROW;
        }
        if let Some(model) = &card.text.model {
            let x = rect.pos.x + CARD_PAD;
            self.draw_text.color = rgba(tokens.ink);
            self.draw_text.draw_abs(cx, dvec2(x, y + 2.0), &clip(&model.line, 40));
            y += ROW;
            self.draw_text.color = rgba(tokens.ink_dim);
            self.draw_text.draw_abs(cx, dvec2(x, y + 2.0), &card.detail);
            y += ROW;
            // The body switch: a choice not offered shows why in its place.
            let mut at = x;
            self.draw_text.text_style.font_size = 9.0;
            for (i, (pick, offered)) in BodyPick::ALL.into_iter().zip(&model.body.picks).enumerate() {
                let word = match offered { Ok(_) => pick.word(), Err(why) => why };
                let text_width = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), word).size_in_lpxs.width as f64;
                let chip = Rect { pos: dvec2(at, y + 1.0), size: dvec2(text_width + 12.0, CHIP_ROW - 4.0) };
                let current = model.body.current == Some(pick);
                self.draw_card_field.color = if current { mix_rgba(tokens.canvas, tokens.selection, 0.35, 1.0) } else { mix_rgba(tokens.canvas, tokens.ink, 0.14, 1.0) };
                self.draw_card_field.draw_abs(cx, chip);
                cx.add_rect_area(&mut card.body_areas[i], chip);
                self.draw_text.color = rgba(if offered.is_ok() { tokens.ink } else { tokens.ink_dim });
                self.draw_text.draw_abs(cx, chip.pos + dvec2(6.0, 2.0), word);
                at += chip.size.x + 3.0;
            }
            self.draw_text.text_style.font_size = 9.5;
            y += CHIP_ROW;
            if let Some((chosen, on)) = model.get_from {
                let mut at = x;
                for (i, ((word, chip_width), choice)) in FROM_CHIPS.into_iter().zip(Choice::ALL).enumerate() {
                    let chip = Rect { pos: dvec2(at, y + 1.0), size: dvec2(chip_width, CHIP_ROW - 4.0) };
                    let picked = choice == chosen;
                    self.draw_card_field.color = if picked { mix_rgba(tokens.canvas, tokens.selection, 0.35, 1.0) } else { mix_rgba(tokens.canvas, tokens.ink, 0.14, 1.0) };
                    self.draw_card_field.draw_abs(cx, chip);
                    cx.add_rect_area(&mut card.from_areas[i], chip);
                    self.draw_text.color = rgba(if on[i] { tokens.ink } else { tokens.ink_dim });
                    self.draw_text.draw_abs(cx, chip.pos + dvec2(6.0, 2.0), word);
                    at += chip_width + 3.0;
                }
                y += CHIP_ROW;
            }
            let link = Rect { pos: dvec2(x, y), size: dvec2(width, ROW) };
            cx.add_rect_area(&mut card.licences_area, link);
            self.draw_text.color = rgba(tokens.selection);
            self.draw_text.draw_abs(cx, dvec2(x, y + 2.0), "3D model sources and licences ›");
        }
        if editing_reference { self.draw_name_field(cx, scope, reference_box); }
        cx.pop_clip_rect();
    }

    /// Send one edit and keep its label and the host's refusal sequence. O(label length).
    pub(super) fn send_card_edit(&mut self, cx: &mut Cx, label: String, op: DesignOp) {
        self.send_card_ops(cx, label, vec![op]);
    }

    /// Send one commit of `ops` as [`Self::send_card_edit`] does.
    pub(super) fn send_card_ops(&mut self, cx: &mut Cx, label: String, ops: Vec<DesignOp>) {
        self.send_card_plan(cx, label, ops, None);
    }

    pub(super) fn send_card_plan(&mut self, cx: &mut Cx, label: String, ops: Vec<DesignOp>, call: Option<super::Invocation>) {
        if let Some(card) = &mut self.card {
            let seen = cx.get_global_ref::<ElecLive>().and_then(|live| live.snap.last_refusal.as_ref()).map_or(0, |r| r.seq);
            card.sent = Some((label.clone(), seen));
            card.note = None;
        }
        let action = ViewAction::Ops { place: vec![], label, ops };
        super::Invocation::send(cx, self.uid, match call { Some(call) => call.attach(action), None => action });
    }

    /// This computer's choice of where it gets the card's KiCad model, the same call as
    /// `elec.part.model.from`: the app keeps it and asks for the model again; a source that is off
    /// is refused in the card. One model-board lock.
    pub(super) fn choose_model(&mut self, cx: &mut Cx, choice: Choice) {
        let Some(card) = &mut self.card else { return };
        let Some(path) = card.text.model.as_ref().and_then(|model| model.path.clone()) else { return };
        card.note = model_board::choose(&path, choice).err().map(|refused| refused_text(&refused));
        self.draw_bg.redraw(cx);
    }

    /// Switch the card's part to `pick` (`e.body`, one undo step), the same commit as
    /// `elec.part.body.set`; a choice not offered or already drawn says why in the card. O(the
    /// part's fields) plus a scan of the document names.
    pub(super) fn pick_body(&mut self, cx: &mut Cx, pick: BodyPick) {
        let Some(card) = &self.card else { return };
        let Some(design) = self.design.clone() else { return };
        let Some(unit) = design.units.get(self.unit) else { return };
        let Some(comp) = unit.circuit.comps.get(card.node as usize) else { return };
        let live = cx.get_global_ref::<ElecLive>();
        let site = live.and_then(|live| live.snap.source_map.as_ref()).and_then(|map| map.body_site(unit.id, &comp.part.name));
        let models = fab_elec_lang::models_doc_name(&unit.name);
        let models_doc = live.and_then(|live| live.snap.docs.as_ref()).is_some_and(|docs| docs.texts.iter().any(|text| text.name == models));
        match body_plan(unit.id, &unit.name, comp, site, models_doc, pick) {
            Ok((label, ops)) => self.send_card_ops(cx, label, ops),
            Err(why) => {
                if let Some(card) = &mut self.card { card.note = Some(why); }
                self.draw_bg.redraw(cx);
            }
        }
    }

    /// Show a new host refusal on the card that submitted the edit. One lookup per signal.
    pub(super) fn card_refusal(&mut self, cx: &mut Cx) {
        let Some(card) = &mut self.card else { return };
        let Some((label, seen)) = &card.sent else { return };
        let Some(refusal) = cx.get_global_ref::<ElecLive>().and_then(|live| live.snap.last_refusal.as_ref()) else { return };
        if refusal.seq <= *seen { return; }
        if refusal.label == *label {
            card.note = Some(refusal.reason.clone());
            self.draw_bg.redraw(cx);
        }
        card.sent = None;
    }

    /// Commit `text` as the edited part's new value (`CircuitOp::SetValue`, written back to its
    /// source; a playing run hears it), or say in the card why it is no value.
    pub(super) fn commit_value_text(&mut self, cx: &mut Cx, text: &str) {
        let Some(edit) = &self.value_edit else { return };
        match parse_value(text, &edit.current, &edit.kind) {
            Ok(value) => {
                let label = format!("Set {} to {}", edit.refdes, value.text);
                let op = DesignOp::Circuit { unit: edit.unit, op: CircuitOp::SetValue { comp: edit.comp, value } };
                self.send_card_edit(cx, label, op);
                self.close_value_edit(cx);
            }
            Err(error) => {
                self.value_combo.as_combo_box().set_error(cx, Some(&error));
                self.draw_bg.redraw(cx);
            }
        }
    }
}
