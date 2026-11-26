//! An opened graphic's inspector: the selected layer's properties in
//! sections, each row a name, its value fields and key navigation (design
//! §4.4). Numbers scrub (drag: 1 pt a step; shift finer, cmd coarser) and
//! type (click, Enter commits, Esc reverts); colours and text type; choices
//! are chips. A keyed property shows its value at the playhead in the
//! accent ink, and editing it writes a key there.
//!
//! The widget holds only gesture state; the owner fills its sections and
//! answers each commit with the next ones.

use makepad_library_browser::draw::DrawEditBlock;
use makepad_widgets::badge::advance;
use makepad_widgets::*;

const ROW: f64 = 26.0;
const HEAD: f64 = 28.0;
const NAME: f64 = 92.0;
const NAV: f64 = 58.0;
const PAD: f64 = 12.0;
/// A line of words in a field, and the most lines a field grows to (it
/// scrolls past them).
const LINE: f64 = 17.0;
const MAX_LINES: usize = 6;

/// A words field's box height for `text`.
fn words_height(text: &str) -> f64 {
    let lines = text.split('\n').count().clamp(1, MAX_LINES);
    lines as f64 * LINE + 10.0
}

/// How a field shows and edits its value.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldKind {
    /// Numbers (one per component), shown `× scale` with `unit`, scrubbed
    /// by `step` per point.
    Number { step: f64, scale: f64, unit: &'static str },
    Color,
    /// One line (a name, a font).
    Text,
    /// Words that may run over lines (a Text's text, a caption, a title):
    /// every line shows; typing, Enter is a new line and each keystroke is
    /// live; Esc, cmd-Enter or leaving the box ends the edit.
    Words,
    Choice(Vec<String>),
    /// A font (a `Font{…}`'s family): shown as its family, opened in the
    /// font picker.
    Font,
    /// Seconds, shown as timecode.
    Time,
    /// Shown, not edited here (a function, a sequence): its text.
    Fixed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// What the owner calls it (a property name, or `span.start` etc.).
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    pub values: Vec<f64>,
    pub text: String,
    /// Keyed: the value shown is the playhead's.
    pub animated: bool,
    pub on_key: bool,
    pub prev_key: bool,
    pub next_key: bool,
    /// Can take keys (shows the key navigation).
    pub keyable: bool,
}

impl Field {
    pub fn number(id: &str, label: &str, values: Vec<f64>, step: f64, scale: f64, unit: &'static str) -> Self {
        Self { id: id.into(), label: label.into(), kind: FieldKind::Number { step, scale, unit }, values, text: String::new(), animated: false, on_key: false, prev_key: false, next_key: false, keyable: false }
    }

    pub fn text(id: &str, label: &str, text: String) -> Self {
        Self { id: id.into(), label: label.into(), kind: FieldKind::Text, values: Vec::new(), text, animated: false, on_key: false, prev_key: false, next_key: false, keyable: false }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Section {
    pub title: String,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Default)]
pub enum InspectorAction {
    #[default]
    None,
    /// Numbers let go, typed or stepped.
    Numbers { id: String, values: Vec<f64> },
    /// Numbers as they are scrubbed (each step of the drag; `Numbers`
    /// follows at the release).
    NumbersLive { id: String, values: Vec<f64> },
    /// A text committed (for words: the end of the edit).
    Text { id: String, text: String },
    /// Words as they are typed (each keystroke; the owner shows them live
    /// and keeps the edit one undo step until its `Text`).
    TextLive { id: String, text: String },
    Choice { id: String, choice: String },
    /// The diamond: add a key at the playhead, or remove the one there.
    KeyToggle(String),
    /// To the previous (-1) or next (1) key.
    KeyNav(String, i32),
    /// A colour's swatch or a font pressed: its picker (`value_pick.rs`).
    Pick { id: String, label: String },
}

#[derive(Clone, Debug, PartialEq)]
enum Spot {
    Value { section: usize, field: usize, part: usize },
    Key { section: usize, field: usize, dir: i32 },
    Choice { section: usize, field: usize, choice: usize },
    /// A choice of many, shown as its value: opens its menu.
    Menu { section: usize, field: usize },
    /// A colour's swatch or a font: opens its picker.
    Pick { section: usize, field: usize },
}

/// Choices shown side by side up to this many; more are a menu.
const CHIPS_MAX: usize = 4;
const MENU_ROW: f64 = 20.0;

#[derive(Script, ScriptHook, Widget)]
pub struct MotionInspector {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_rect: DrawColor,
    #[live]
    draw_field: DrawEditBlock,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_value: DrawText,
    #[live]
    draw_caps: DrawText,
    #[live]
    draw_badge: DrawText,
    #[live]
    editor: TextInput,
    /// The box words are typed in (multi-line).
    #[live]
    note: TextInput,
    #[rust]
    sections: Vec<Section>,
    #[rust]
    title: String,
    #[rust]
    subtitle: String,
    /// The role chip's hue in the header (a layer), none for the document.
    #[rust]
    hue: Option<u32>,
    #[rust]
    rect: Rect,
    #[rust]
    scroll: f64,
    /// Spots drawn this frame, for hits.
    #[rust]
    spots: Vec<(Rect, Spot)>,
    /// A press on a value: where, the values then, and the scrub so far.
    #[rust]
    press: Option<(Spot, Vec2d, Vec<f64>, bool)>,
    /// A scrub's live values.
    #[rust]
    scrubbing: Option<(usize, usize, Vec<f64>)>,
    /// A field being typed into, and its box.
    #[rust]
    typing: Option<(usize, usize, usize, Rect)>,
    #[rust]
    hover: Option<Vec2d>,
    #[rust]
    focus_pending: bool,
    /// The open menu of a choice: its field and the well it hangs from.
    #[rust]
    menu: Option<(usize, usize, Rect)>,
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.MotionInspectorBase = #(MotionInspector::register_widget(vm))
    mod.widgets.MotionInspector = set_type_default() do mod.widgets.MotionInspectorBase{
        width: Fill height: Fill
        draw_bg +: {color: stage.panel}
        draw_rect +: {color: stage.separator}
        draw_label +: {color: stage.label text_style: theme.font_regular{font_size: 9}}
        draw_value +: {color: stage.text_bright text_style: theme.font_regular{font_size: 9}}
        draw_caps +: {color: stage.secondary text_style: theme.font_bold{font_size: 7.5}}
        draw_badge +: {color: stage.secondary text_style: theme.font_regular{font_size: 7}}
        editor: TextInput{width: 100 height: 22 margin: 0 padding: Inset{left: 6 right: 6 top: 3 bottom: 3} empty_text: ""
            draw_bg +: {color: stage.well color_hover: stage.well color_focus: stage.well border_color: stage.cursor border_size: 1.0}
            draw_text +: {text_style: theme.font_regular{font_size: 9}}}
        note: TextInput{width: 100 height: 22 margin: 0 padding: Inset{left: 6 right: 6 top: 4 bottom: 4} empty_text: ""
            is_multiline: true
            draw_bg +: {color: stage.well color_hover: stage.well color_focus: stage.well border_color: stage.cursor border_size: 1.0}
            draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    }
}

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut color = Vec4f::from_u32(value);
    color.w *= alpha;
    color
}

fn fmt_scaled(value: f64, scale: f64) -> String {
    let shown = value * scale;
    let rounded = (shown * 100.0).round() / 100.0;
    let mut text = format!("{rounded:.2}");
    while text.contains('.') && (text.ends_with('0') || text.ends_with('.')) {
        text.pop();
    }
    if text == "-0" { "0".into() } else { text.replace('-', "−") }
}

impl MotionInspector {
    pub fn set(&mut self, cx: &mut Cx, title: &str, subtitle: &str, hue: Option<u32>, sections: Vec<Section>) {
        self.hue = hue;
        // Another layer: an open menu was the other one's.
        if self.title != title {
            self.menu = None;
        }
        if self.press.is_some() || self.typing.is_some() {
            // A gesture keeps its field; the rest follows on its release.
            self.title = title.to_string();
            self.subtitle = subtitle.to_string();
            return;
        }
        self.title = title.to_string();
        self.subtitle = subtitle.to_string();
        self.sections = sections;
        self.redraw(cx);
    }

    fn field(&self, section: usize, field: usize) -> Option<&Field> {
        self.sections.get(section)?.fields.get(field)
    }

    fn spot_at(&self, abs: Vec2d) -> Option<Spot> {
        self.spots.iter().rev().find(|(rect, _)| rect.contains(abs)).map(|(_, spot)| spot.clone())
    }

    fn begin_typing(&mut self, cx: &mut Cx, section: usize, field: usize, part: usize, rect: Rect) {
        let Some(f) = self.field(section, field).cloned() else { return };
        let text = match &f.kind {
            FieldKind::Number { scale, .. } => f.values.get(part).map(|v| fmt_scaled(*v, *scale).replace('−', "-")).unwrap_or_default(),
            FieldKind::Time => f.values.first().map(|v| makepad_motion::patch::fmt_time(*v)).unwrap_or_default(),
            _ => f.text.clone(),
        };
        self.typing = Some((section, field, part, rect));
        if f.kind == FieldKind::Words {
            self.note.set_text(cx, &text);
        } else {
            self.editor.set_text(cx, &text);
        }
        self.focus_pending = true;
                self.redraw(cx);
    }

    fn end_typing(&mut self, cx: &mut Cx, commit: bool) {
        let Some((section, field, part, _)) = self.typing.take() else { return };
        self.redraw(cx);
        if !commit {
            return;
        }
        let Some(f) = self.field(section, field).cloned() else { return };
        let text = if f.kind == FieldKind::Words { self.note.text() } else { self.editor.text() };
        let uid = self.widget_uid();
        match &f.kind {
            FieldKind::Number { scale, .. } => {
                let Ok(value) = text.trim().replace('−', "-").trim_end_matches(['%', '°', ' ']).parse::<f64>() else { return };
                let mut values = f.values.clone();
                if let Some(slot) = values.get_mut(part) {
                    *slot = value / scale;
                }
                cx.widget_action(uid, InspectorAction::Numbers { id: f.id, values });
            }
            FieldKind::Time => {
                let Ok(value) = text.trim().parse::<f64>() else { return };
                cx.widget_action(uid, InspectorAction::Numbers { id: f.id, values: vec![value] });
            }
            FieldKind::Color | FieldKind::Text | FieldKind::Words => cx.widget_action(uid, InspectorAction::Text { id: f.id, text }),
            _ => {}
        }
    }

    fn row_name(&mut self, cx: &mut Cx2d, rect: Rect, name: &str, ink: Vec4f) {
        let text = fit(&self.draw_label, cx, name, NAME - 8.0);
        self.draw_label.color = ink;
        self.draw_label.draw_abs(cx, dvec2(rect.pos.x + PAD, rect.pos.y + (ROW - 12.0) * 0.5), &text);
    }

    fn well(&mut self, cx: &mut Cx2d, rect: Rect, fill: u32, edge: u32) {
        self.draw_field.fill = color(fill, 1.0);
        self.draw_field.edge = color(edge, 1.0);
        self.draw_field.stroke = 1.0;
        self.draw_field.radius = 2.5;
        self.draw_field.shape = 0.0;
        self.draw_field.draw_abs(cx, rect);
    }

    fn diamond(&mut self, cx: &mut Cx2d, rect: Rect, fill: Vec4f, edge: Vec4f) {
        self.draw_field.fill = fill;
        self.draw_field.edge = edge;
        self.draw_field.stroke = 1.2;
        self.draw_field.radius = 0.0;
        self.draw_field.shape = 1.0;
        self.draw_field.draw_abs(cx, rect);
    }
}

/// `text` clipped to `width` with an ellipsis.
fn fit(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64) -> String {
    if advance(draw, cx, text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate = format!("{}…", chars[..mid].iter().collect::<String>());
        if advance(draw, cx, &candidate) <= width { lo = mid } else { hi = mid - 1 }
    }
    if lo == 0 { String::new() } else { format!("{}…", chars[..lo].iter().collect::<String>()) }
}

impl Widget for MotionInspector {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let uid = self.widget_uid();
        let words = self.typing.and_then(|(s, f, _, _)| self.field(s, f).map(|f| f.kind == FieldKind::Words));
        if words == Some(true) {
            let note_uid = self.note.widget_uid();
            self.note.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                for action in actions.filter_widget_actions(note_uid) {
                    match action.cast::<TextInputAction>() {
                        TextInputAction::Changed(text) => {
                            if let Some((s, f, _, _)) = self.typing {
                                if let Some(field) = self.field(s, f).cloned() {
                                    cx.widget_action(uid, InspectorAction::TextLive { id: field.id, text });
                                }
                            }
                            // The box grows with its lines.
                            self.redraw(cx);
                        }
                        // Plain Enter is a new line; these end the edit,
                        // keeping what was typed (it is already live).
                        TextInputAction::Returned(..) | TextInputAction::Escaped | TextInputAction::KeyFocusLost => self.end_typing(cx, true),
                        _ => {}
                    }
                }
            }
        } else if words == Some(false) {
            let editor_uid = self.editor.widget_uid();
            self.editor.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                for action in actions.filter_widget_actions(editor_uid) {
                    match action.cast::<TextInputAction>() {
                        TextInputAction::Returned(..) => self.end_typing(cx, true),
                        TextInputAction::Escaped => self.end_typing(cx, false),
                        _ => {}
                    }
                }
            }
        }
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerScroll(fs) => {
                self.scroll = (self.scroll + fs.scroll.y).max(0.0);
                self.redraw(cx);
            }
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                self.hover = Some(fe.abs);
                let cursor = match self.spot_at(fe.abs) {
                    Some(Spot::Value { section, field, .. }) if matches!(self.field(section, field).map(|f| &f.kind), Some(FieldKind::Number { .. } | FieldKind::Time)) => MouseCursor::EwResize,
                    Some(Spot::Value { .. }) => MouseCursor::Text,
                    Some(_) => MouseCursor::Hand,
                    None => MouseCursor::Default,
                };
                cx.set_cursor(cursor);
                self.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = None;
                self.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                if self.typing.is_some_and(|(_, _, _, r)| r.contains(fe.abs)) {
                    // In the box being typed in: the box's own.
                    return;
                }
                if self.typing.is_some() {
                    self.end_typing(cx, true);
                }
                let spot = self.spot_at(fe.abs);
                if self.menu.is_some() && !matches!(spot, Some(Spot::Choice { .. })) {
                    self.menu = None;
                    self.redraw(cx);
                    return;
                }
                let Some(spot) = spot else { return };
                let values = match &spot {
                    Spot::Value { section, field, .. } => self.field(*section, *field).map(|f| f.values.clone()).unwrap_or_default(),
                    _ => Vec::new(),
                };
                self.press = Some((spot, fe.abs, values, false));
            }
            Hit::FingerMove(fe) => {
                let Some((Spot::Value { section, field, part }, origin, start, _)) = self.press.clone() else { return };
                let Some(f) = self.field(section, field).cloned() else { return };
                let step = match f.kind {
                    FieldKind::Number { step, .. } => step,
                    FieldKind::Time => 1.0 / 60.0,
                    _ => return,
                };
                let dx = fe.abs.x - origin.x;
                if dx.abs() < 3.0 && self.scrubbing.is_none() {
                    return;
                }
                let factor = if fe.modifiers.shift { 0.1 } else if fe.modifiers.logo || fe.modifiers.control { 10.0 } else { 1.0 };
                let mut values = start.clone();
                if let Some(slot) = values.get_mut(part) {
                    *slot += (dx * step * factor / step).round() * step;
                }
                if let Some(f) = self.field(section, field) {
                    cx.widget_action(self.widget_uid(), InspectorAction::NumbersLive { id: f.id.clone(), values: values.clone() });
                }
                self.scrubbing = Some((section, field, values));
                if let Some(press) = &mut self.press {
                    press.3 = true;
                }
                self.redraw(cx);
            }
            Hit::FingerUp(fe) => {
                let Some((spot, _, _, moved)) = self.press.take() else { return };
                self.redraw(cx);
                match spot {
                    Spot::Value { section, field, part } => {
                        if moved {
                            if let Some((_, _, values)) = self.scrubbing.take() {
                                if let Some(f) = self.field(section, field).cloned() {
                                    cx.widget_action(uid, InspectorAction::Numbers { id: f.id, values });
                                }
                            }
                        } else if fe.is_over {
                            let rect = self.spots.iter().find(|(_, s)| *s == Spot::Value { section, field, part }).map(|(rect, _)| *rect);
                            if let Some(rect) = rect {
                                let is_fixed = matches!(self.field(section, field).map(|f| &f.kind), Some(FieldKind::Fixed | FieldKind::Choice(_)));
                                if !is_fixed {
                                    self.begin_typing(cx, section, field, part, rect);
                                }
                            }
                        }
                    }
                    Spot::Key { section, field, dir } if fe.is_over => {
                        if let Some(f) = self.field(section, field).cloned() {
                            if dir == 0 {
                                cx.widget_action(uid, InspectorAction::KeyToggle(f.id));
                            } else {
                                cx.widget_action(uid, InspectorAction::KeyNav(f.id, dir));
                            }
                        }
                    }
                    Spot::Pick { section, field } if fe.is_over => {
                        if let Some(f) = self.field(section, field).cloned() {
                            cx.widget_action(uid, InspectorAction::Pick { id: f.id, label: f.label });
                        }
                    }
                    Spot::Menu { section, field } if fe.is_over => {
                        let rect = self.spots.iter().find(|(_, s)| *s == (Spot::Menu { section, field })).map(|(rect, _)| *rect);
                        self.menu = rect.map(|rect| (section, field, rect));
                    }
                    Spot::Choice { section, field, choice } if fe.is_over => {
                        self.menu = None;
                        if let Some(f) = self.field(section, field).cloned() {
                            if let FieldKind::Choice(choices) = &f.kind {
                                if let Some(choice) = choices.get(choice) {
                                    cx.widget_action(uid, InspectorAction::Choice { id: f.id.clone(), choice: choice.clone() });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let step_t0 = makepad_value_pickers::panel::host::timing().then(Cx::time_now);
        let step = self.draw_walk_timed(cx, scope, walk);
        if let Some(t0) = step_t0 {
            makepad_value_pickers::panel::host::time_add("draw.inspector", Cx::time_now() - t0);
        }
        step
    }
}

impl MotionInspector {
    fn draw_walk_timed(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = color(p.panel, 1.0);
        self.draw_bg.draw_abs(cx, rect);
        self.spots.clear();
        cx.push_clip_rect(rect);
        // The header (§3.5): a role chip, the name, and the kind line.
        let mut y = rect.pos.y + 10.0 - self.scroll;
        let mut text_x = rect.pos.x + PAD;
        if let Some(hue) = self.hue {
            let strip = super::session::mix(hue, p.wave_bg, 0.5);
            self.draw_field.fill = color(strip, 1.0);
            self.draw_field.edge = color(hue, 0.7);
            self.draw_field.stroke = 1.0;
            self.draw_field.radius = 3.0;
            self.draw_field.shape = 0.0;
            self.draw_field.draw_abs(cx, Rect { pos: dvec2(text_x, y + 3.0), size: dvec2(18.0, 18.0) });
            text_x += 26.0;
        }
        let title = fit(&self.draw_value, cx, &self.title, rect.pos.x + rect.size.x - PAD - text_x);
        self.draw_value.color = color(p.text_hi, 1.0);
        self.draw_value.draw_abs(cx, dvec2(text_x, y), &title);
        y += 16.0;
        self.draw_label.color = color(p.muted, 1.0);
        let subtitle = fit(&self.draw_label, cx, &self.subtitle, rect.pos.x + rect.size.x - PAD - text_x);
        self.draw_label.draw_abs(cx, dvec2(text_x, y), &subtitle);
        y += 22.0;
        let sections = self.sections.clone();
        for (si, section) in sections.iter().enumerate() {
            self.draw_rect.color = color(p.line_faint, 1.0);
            self.draw_rect.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, 1.0) });
            self.draw_caps.color = color(p.secondary, 1.0);
            self.draw_caps.draw_abs(cx, dvec2(rect.pos.x + PAD, y + (HEAD - 10.0) * 0.5), &section.title.to_uppercase());
            y += HEAD;
            for (fi, field) in section.fields.iter().enumerate() {
                let row = Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, ROW) };
                self.row_name(cx, row, &field.label, color(p.label, 1.0));
                let value_x = rect.pos.x + PAD + NAME;
                let value_w = (rect.size.x - PAD - NAME - NAV - PAD).max(40.0);
                let ink = if field.animated { color(p.accent_soft, 1.0) } else { color(p.text_bright, 1.0) };
                match &field.kind {
                    FieldKind::Number { scale, unit, .. } => {
                        let n = field.values.len().max(1);
                        let w = (value_w - 4.0 * (n as f64 - 1.0)) / n as f64;
                        let values = match &self.scrubbing {
                            Some((s, f, values)) if *s == si && *f == fi => values.clone(),
                            _ => field.values.clone(),
                        };
                        for (part, value) in values.iter().enumerate() {
                            let r = Rect { pos: dvec2(value_x + part as f64 * (w + 4.0), y + 2.0), size: dvec2(w, ROW - 4.0) };
                            let scrubbing = self.scrubbing.as_ref().is_some_and(|(s, f, _)| *s == si && *f == fi);
                            let hovered = self.hover.is_some_and(|hover| r.contains(hover));
                            self.well(cx, r, if scrubbing { p.accent_wash } else { p.well }, if scrubbing { p.accent } else if hovered { p.border_hi } else { p.line });
                            if self.typing.as_ref().is_some_and(|(s, f, pt, _)| *s == si && *f == fi && *pt == part) {
                                self.spots.push((r, Spot::Value { section: si, field: fi, part }));
                                continue;
                            }
                            let text = fmt_scaled(*value, *scale);
                            let unit_w = if unit.is_empty() { 0.0 } else { advance(&self.draw_label, cx, unit) + 3.0 };
                            let tw = advance(&self.draw_value, cx, &text);
                            self.draw_value.color = ink;
                            self.draw_value.draw_abs(cx, dvec2(r.pos.x + r.size.x - 6.0 - unit_w - tw, r.pos.y + 4.0), &text);
                            if !unit.is_empty() {
                                self.draw_label.color = color(p.muted, 1.0);
                                self.draw_label.draw_abs(cx, dvec2(r.pos.x + r.size.x - 6.0 - unit_w + 3.0, r.pos.y + 4.5), unit);
                            }
                            self.spots.push((r, Spot::Value { section: si, field: fi, part }));
                        }
                    }
                    FieldKind::Time => {
                        let r = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(value_w, ROW - 4.0) };
                        self.well(cx, r, p.well, p.line);
                        if !self.typing.as_ref().is_some_and(|(s, f, _, _)| *s == si && *f == fi) {
                            let value = match &self.scrubbing {
                                Some((s, f, values)) if *s == si && *f == fi => values.first().copied(),
                                _ => field.values.first().copied(),
                            };
                            let text = format!("{} s", value.map(makepad_motion::patch::fmt_time).unwrap_or_default());
                            let tw = advance(&self.draw_value, cx, &text);
                            self.draw_value.color = ink;
                            self.draw_value.draw_abs(cx, dvec2(r.pos.x + r.size.x - 6.0 - tw, r.pos.y + 4.0), &text);
                        }
                        self.spots.push((r, Spot::Value { section: si, field: fi, part: 0 }));
                    }
                    FieldKind::Color => {
                        let swatch = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(22.0, ROW - 4.0) };
                        let rgba = if field.values.len() == 4 { vec4(field.values[0] as f32, field.values[1] as f32, field.values[2] as f32, field.values[3] as f32) } else { color(0x808080ff, 1.0) };
                        self.draw_field.fill = rgba;
                        self.draw_field.edge = color(p.border_hi, 1.0);
                        self.draw_field.stroke = 1.0;
                        self.draw_field.radius = 2.5;
                        self.draw_field.shape = 0.0;
                        self.draw_field.draw_abs(cx, swatch);
                        self.spots.push((swatch, Spot::Pick { section: si, field: fi }));
                        let r = Rect { pos: dvec2(value_x + 28.0, y + 2.0), size: dvec2(value_w - 28.0, ROW - 4.0) };
                        self.well(cx, r, p.well, p.line);
                        if !self.typing.as_ref().is_some_and(|(s, f, _, _)| *s == si && *f == fi) {
                            self.draw_value.color = ink;
                            self.draw_value.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 4.0), &field.text);
                        }
                        self.spots.push((r, Spot::Value { section: si, field: fi, part: 0 }));
                    }
                    FieldKind::Words => {
                        let typing_here = self.typing.as_ref().is_some_and(|(s, f, _, _)| *s == si && *f == fi);
                        let shown = if typing_here { self.note.text() } else { field.text.clone() };
                        let r = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(value_w + if field.keyable { 0.0 } else { NAV }, words_height(&shown)) };
                        self.well(cx, r, p.well, p.line);
                        if typing_here {
                            // The box follows its lines as they are typed.
                            if let Some(typing) = &mut self.typing {
                                typing.3 = r;
                            }
                        } else {
                            self.draw_value.color = ink;
                            let lines: Vec<&str> = shown.split('\n').collect();
                            for (li, line) in lines.iter().take(MAX_LINES).enumerate() {
                                let more = li + 1 == MAX_LINES && lines.len() > MAX_LINES;
                                let line = if more { format!("{line} …") } else { line.to_string() };
                                let line = fit(&self.draw_value, cx, &line, r.size.x - 12.0);
                                self.draw_value.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 5.0 + li as f64 * LINE), &line);
                            }
                        }
                        self.spots.push((r, Spot::Value { section: si, field: fi, part: 0 }));
                        y += (r.size.y + 4.0 - ROW).max(0.0);
                    }
                    FieldKind::Text | FieldKind::Fixed => {
                        let r = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(value_w + if field.keyable { 0.0 } else { NAV }, ROW - 4.0) };
                        let fixed = field.kind == FieldKind::Fixed;
                        self.well(cx, r, if fixed { p.panel } else { p.well }, p.line);
                        if !self.typing.as_ref().is_some_and(|(s, f, _, _)| *s == si && *f == fi) {
                            let text = fit(&self.draw_value, cx, &field.text.replace('\n', " ⏎ "), r.size.x - 12.0);
                            self.draw_value.color = if fixed { color(p.muted, 1.0) } else { ink };
                            self.draw_value.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 4.0), &text);
                        }
                        self.spots.push((r, Spot::Value { section: si, field: fi, part: 0 }));
                    }
                    FieldKind::Font => {
                        // The family, and "Aa": a press opens the font picker.
                        let well = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(value_w + NAV, ROW - 4.0) };
                        self.well(cx, well, p.well, p.line);
                        let shown = if field.text.is_empty() { "—".to_string() } else { field.text.clone() };
                        let text = fit(&self.draw_value, cx, &shown, well.size.x - 30.0);
                        self.draw_value.color = color(p.text, 1.0);
                        self.draw_value.draw_abs(cx, dvec2(well.pos.x + 6.0, well.pos.y + 4.0), &text);
                        self.draw_label.color = color(p.accent, 1.0);
                        self.draw_label.draw_abs(cx, dvec2(well.pos.x + well.size.x - 20.0, well.pos.y + 3.5), "Aa");
                        self.spots.push((well, Spot::Pick { section: si, field: fi }));
                    }
                    FieldKind::Choice(choices) if choices.len() > CHIPS_MAX => {
                        // Many choices: the value, and its menu.
                        let well = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(value_w + NAV, ROW - 4.0) };
                        let open = self.menu.is_some_and(|(s, f, _)| s == si && f == fi);
                        self.well(cx, well, p.well, if open { p.accent } else { p.line });
                        let shown = if field.text.is_empty() { "—".to_string() } else { field.text.clone() };
                        let text = fit(&self.draw_value, cx, &shown, well.size.x - 24.0);
                        self.draw_value.color = color(p.text, 1.0);
                        self.draw_value.draw_abs(cx, dvec2(well.pos.x + 6.0, well.pos.y + 4.0), &text);
                        self.draw_label.color = color(p.label, 1.0);
                        self.draw_label.draw_abs(cx, dvec2(well.pos.x + well.size.x - 14.0, well.pos.y + 3.5), "▾");
                        self.spots.push((well, Spot::Menu { section: si, field: fi }));
                    }
                    FieldKind::Choice(choices) => {
                        let n = choices.len().max(1);
                        let full = value_w + NAV;
                        let w = full / n as f64;
                        let well = Rect { pos: dvec2(value_x, y + 2.0), size: dvec2(full, ROW - 4.0) };
                        self.well(cx, well, p.well, p.line);
                        for (ci, choice) in choices.iter().enumerate() {
                            let r = Rect { pos: dvec2(value_x + ci as f64 * w + 1.0, y + 3.0), size: dvec2(w - 2.0, ROW - 6.0) };
                            let active = *choice == field.text;
                            if active {
                                self.well(cx, r, p.accent_wash, p.accent_wash);
                            }
                            let label = fit(&self.draw_label, cx, choice, r.size.x - 4.0);
                            let tw = advance(&self.draw_label, cx, &label);
                            self.draw_label.color = color(if active { p.text_hi } else { p.label }, 1.0);
                            self.draw_label.draw_abs(cx, dvec2(r.pos.x + (r.size.x - tw) * 0.5, r.pos.y + 3.5), &label);
                            self.spots.push((r, Spot::Choice { section: si, field: fi, choice: ci }));
                        }
                    }
                }
                // Key navigation: ◀ ◆ ▶.
                if field.keyable {
                    let nav_x = rect.pos.x + rect.size.x - PAD - NAV + 4.0;
                    let cy = y + ROW * 0.5;
                    for (dir, x) in [(-1, nav_x), (0, nav_x + 18.0), (1, nav_x + 40.0)] {
                        let r = Rect { pos: dvec2(x, y + 2.0), size: dvec2(16.0, ROW - 4.0) };
                        if dir == 0 {
                            let d = Rect { pos: dvec2(x + 3.0, cy - 5.0), size: dvec2(10.0, 10.0) };
                            let (fill, edge) = if field.on_key {
                                (color(p.accent, 1.0), color(p.accent, 1.0))
                            } else if field.animated {
                                (color(0, 0.0), color(p.accent_soft, 1.0))
                            } else {
                                (color(0, 0.0), color(p.faint, 1.0))
                            };
                            self.diamond(cx, d, fill, edge);
                        } else if field.animated {
                            let has = if dir < 0 { field.prev_key } else { field.next_key };
                            let glyph = if dir < 0 { "‹" } else { "›" };
                            self.draw_value.color = color(if has { p.label } else { p.dim }, 1.0);
                            self.draw_value.draw_abs(cx, dvec2(x + 4.0, y + 4.0), glyph);
                        }
                        self.spots.push((r, Spot::Key { section: si, field: fi, dir }));
                    }
                }
                y += ROW;
            }
            y += 6.0;
        }
        if self.sections.is_empty() {
            self.draw_label.color = color(p.secondary, 1.0);
            self.draw_label.draw_abs(cx, dvec2(rect.pos.x + PAD, y), "Select a layer, a key or a segment");
        }
        let _ = y;
        // An open menu of choices, under (or over) its well.
        if let Some((si, fi, well)) = self.menu {
            let choices = match self.field(si, fi).map(|f| (&f.kind, f.text.clone())) {
                Some((FieldKind::Choice(choices), current)) => Some((choices.clone(), current)),
                _ => None,
            };
            if let Some((choices, current)) = choices {
                let h = choices.len() as f64 * MENU_ROW + 6.0;
                let below = well.pos.y + well.size.y + 2.0;
                let top = if below + h <= rect.pos.y + rect.size.y { below } else { (well.pos.y - 2.0 - h).max(rect.pos.y) };
                let panel = Rect { pos: dvec2(well.pos.x, top), size: dvec2(well.size.x, h) };
                self.draw_field.fill = color(p.raised, 1.0);
                self.draw_field.edge = color(p.line, 1.0);
                self.draw_field.stroke = 1.0;
                self.draw_field.radius = 4.0;
                self.draw_field.shape = 0.0;
                self.draw_field.draw_abs(cx, panel);
                for (ci, choice) in choices.iter().enumerate() {
                    let r = Rect { pos: dvec2(panel.pos.x + 3.0, panel.pos.y + 3.0 + ci as f64 * MENU_ROW), size: dvec2(panel.size.x - 6.0, MENU_ROW) };
                    let hovered = self.hover.is_some_and(|hover| r.contains(hover));
                    if hovered || *choice == current {
                        self.well(cx, r, if hovered { p.accent_wash } else { p.well }, if hovered { p.accent_wash } else { p.well });
                    }
                    let label = fit(&self.draw_label, cx, choice, r.size.x - 12.0);
                    self.draw_label.color = color(if *choice == current { p.text_hi } else { p.label }, 1.0);
                    self.draw_label.draw_abs(cx, dvec2(r.pos.x + 6.0, r.pos.y + 4.0), &label);
                    self.spots.push((r, Spot::Choice { section: si, field: fi, choice: ci }));
                }
            } else {
                self.menu = None;
            }
        }
        // The typing box over its field.
        if let Some((s, f, _, r)) = self.typing {
            let walk = Walk { abs_pos: Some(r.pos), width: Size::Fixed(r.size.x), height: Size::Fixed(r.size.y), ..Walk::default() };
            let input = if self.field(s, f).is_some_and(|f| f.kind == FieldKind::Words) { &mut self.note } else { &mut self.editor };
            input.draw_walk(cx, scope, walk).ok();
            if std::mem::take(&mut self.focus_pending) {
                // Focus once it has an area: it was just drawn.
                input.take_key_focus(cx);
                input.select_all(cx);
            }
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }
}
