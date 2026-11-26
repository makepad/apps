//! A font at a path in a panel: its row, and the font picker it opens
//! (`PanelFont{path: "Text.font"}`). A face is read from and written as a
//! `Font{family weight italic axes}`.

use crate::font_picker::{FontChoice, FontPickAction, FontPicker};
use makepad_edit_values::number::fmt_num;
use makepad_value_pickers::panel::control::DrawPanelShape;
use makepad_value_pickers::panel::host::{self, PanelValue};
use makepad_value_pickers::panel::rich::{draw_row, named_value, ROW};
use makepad_widgets::makepad_script::syntax::{self as tree, Kind};
use makepad_widgets::makepad_script::ScriptFnRef;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.PanelFontBase = #(PanelFont::register_widget(vm))
    /** A font at a path: its row, and the font picker it opens. */
    mod.widgets.PanelFont = set_type_default() do mod.widgets.PanelFontBase{
        width: Fill height: Fit flow: Down spacing: 4
        draw_text +: {text_style: theme.font_regular{font_size: 8.5}}
        RoundedView{width: Fill height: Fit flow: Down padding: Inset{left: 8 right: 8 top: 8 bottom: 8}
            draw_bg +: {color: stage.well border_color: stage.line_faint border_size: 1.0 border_radius: 5.0}
            picker := mod.widgets.FontPicker{visible: false}}
    }
}

/// A `Font{family weight italic axes}` as written, read.
pub fn font_of(code: &str) -> Option<FontChoice> {
    let node = tree::parse_value(code.trim()).ok()?;
    let (_, fields) = node.object()?;
    let mut choice = FontChoice { family: "Inter".into(), weight: 400.0, italic: false, axes: Vec::new() };
    for f in fields {
        match (f.name.as_deref(), &f.value.kind) {
            (Some("family"), Kind::Str(s)) => choice.family = s.clone(),
            (Some("weight"), Kind::Number(w)) => choice.weight = *w,
            (Some("italic"), Kind::Ident(b)) => choice.italic = b == "true",
            (Some("axes"), Kind::Object { fields, .. }) => choice.axes = fields.iter().filter_map(|a| Some((a.name.clone()?, a.value.number()?))).collect(),
            _ => {}
        }
    }
    Some(choice)
}

/// A face written as a `Font{…}`.
pub fn font_code(choice: &FontChoice) -> String {
    let mut s = format!("Font{{family: {:?} weight: {}", choice.family, fmt_num(choice.weight, 1));
    if choice.italic {
        s.push_str(" italic: true");
    }
    if !choice.axes.is_empty() {
        let axes: Vec<String> = choice.axes.iter().map(|(t, v)| format!("{t}: {}", fmt_num(*v, 1))).collect();
        s.push_str(&format!(" axes: {{{}}}", axes.join(" ")));
    }
    s.push('}');
    s
}

#[derive(Script, ScriptHook, Widget)]
pub struct PanelFont {
    /// When in the film it acts ("beats 9-16"): the host reads it from the
    /// panel's code.
    #[live]
    pub when: String,
    #[rust]
    place: Area,
    #[deref]
    view: View,
    #[live]
    pub path: String,
    #[live]
    pub label: String,
    #[live]
    pub value: String,
    /// The words the families are shown in (else each family's name).
    #[live]
    pub sample: String,
    #[live]
    on_change: ScriptFnRef,
    #[source]
    source: ScriptObjectRef,
    #[live]
    draw_head: DrawPanelShape,
    #[live]
    draw_swatch: DrawPanelShape,
    #[live]
    draw_text: DrawText,
    #[rust]
    code: String,
    /// What the code stands for (a name's face: `SANS` → `Font{…}`).
    #[rust]
    resolved: String,
    #[rust]
    seen: u64,
    /// Opened by hand: brought into view once drawn open.
    #[rust]
    reveal_open: bool,
    #[rust]
    open: bool,
    #[rust]
    hot: bool,
    #[rust]
    moving: bool,
}

impl PanelFont {
    fn read(&mut self, cx: &mut Cx) {
        let rev = host::revision();
        if self.seen == rev && !self.code.is_empty() {
            return;
        }
        self.seen = rev;
        let found = host::read(&self.path).map(|v| v.code()).or_else(|| (!self.value.is_empty()).then(|| self.value.clone()));
        let Some(code) = found else { return };
        let resolved = named_value(&self.path, &code);
        if self.moving || (code == self.code && resolved == self.resolved) {
            return;
        }
        self.code = code;
        self.resolved = resolved;
        // The picker lists the families whatever the face is written as
        // (a name, a `Font{…}`), the face it is chosen.
        let choice = font_of(&self.resolved).unwrap_or(FontChoice { family: "Inter".into(), weight: 400.0, italic: false, axes: Vec::new() });
        if let Some(mut picker) = self.view.widget(cx, ids!(picker)).borrow_mut::<FontPicker>() {
            picker.set_value(cx, choice, &self.sample);
        }
    }

    fn write(&mut self, cx: &mut Cx, choice: &FontChoice) {
        self.code = font_code(choice);
        self.resolved = self.code.clone();
        if host::hosted() && !self.path.is_empty() {
            host::set(&self.path, PanelValue::Code(self.code.clone()));
        }
        if host::bound(&self.on_change) {
            let arg = cx.with_vm(|vm| vm.bx.heap_mut().new_string_from_str(&self.code));
            cx.widget_to_script_call(self.widget_uid(), NIL, self.source.clone(), self.on_change.clone(), &[arg]);
        }
        self.draw_head.redraw(cx);
    }
}

impl Widget for PanelFont {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event.hits(cx, self.draw_head.area()) {
            Hit::FingerHoverIn(_) => {
                self.hot = true;
                cx.set_cursor(MouseCursor::Hand);
                self.draw_head.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hot = false;
                self.draw_head.redraw(cx);
            }
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() => {
                self.open = !self.open;
                self.reveal_open = self.open;
                self.view.widget(cx, ids!(picker)).set_visible(cx, self.open);
                self.view.redraw(cx);
            }
            _ => {}
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let uid = self.view.widget(cx, ids!(picker)).widget_uid();
            for action in actions.filter_widget_actions_cast::<FontPickAction>(uid) {
                match action {
                    FontPickAction::Preview(choice) => {
                        self.moving = true;
                        self.write(cx, &choice);
                    }
                    FontPickAction::Commit(choice) => {
                        self.moving = false;
                        self.write(cx, &choice);
                    }
                    FontPickAction::None => {}
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.read(cx);
        host::register(host::Bound { path: self.path.clone(), kind: host::BoundKind::Other });
        cx.begin_turtle(walk, Layout::flow_down());
        let row = cx.walk_turtle(Walk::new(Size::fill(), Size::Fixed(ROW)));
        let label = if self.label.is_empty() { host::label_of(&self.path) } else { self.label.clone() };
        // A face written as a name (`SANS`) shows the name and its face.
        let face = font_of(&self.resolved).map(|c| format!("{} {}{}", c.family, c.weight as i64, if c.italic { " italic" } else { "" }));
        let name = self.code.trim();
        let shown = match face {
            Some(face) if !name.starts_with("Font") && !name.is_empty() => format!("{name} · {face}"),
            Some(face) => face,
            None => self.code.clone(),
        };
        draw_row(cx, &mut self.draw_head, &mut self.draw_swatch, &mut self.draw_text, row, 0.0, &label, None, &shown, self.open, self.hot);
        if self.open {
            let _ = self.view.draw_walk(cx, scope, Walk::new(Size::fill(), Size::fit()));
        }
        let placed_rect = cx.end_turtle();
        if std::mem::take(&mut self.reveal_open) && self.open {
            host::opened(placed_rect);
        }
        let (uid, paths) = (self.widget_uid().0, vec![self.path.clone()]);
        host::placed(cx, &mut self.place, uid, placed_rect, paths);
        DrawStep::done()
    }
}

