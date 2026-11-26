//! [`FabColorMap`]: the print's colours on the printer. Each [`FabColorLink`] is one colour
//! of the palette: its design swatch, name and whether its slot is saved or picked
//! automatically, then → the slot it prints from (the spool's swatch, the tray, the
//! material) as a dropdown. The Part panel's Colours list uses the same row: a press on the
//! design opens its editor; a press on the slot opens the printer's trays to pick one.
//! Every hint is a hover tooltip; nothing moves under the pointer.
//!
//! It reads `PrintState::colors` (made on printer reports and project changes) and edits only
//! through [`PartAction`]s. It reads the published mapping, never resolves trays while drawing.

use super::colours::{hex, mapping_text, mapping_tip, tray_menu, tray_pick, tray_tip, How, Row, TrayState};
use super::trays::DrawFabChip;
use super::*;
use crate::api::{AppState, PartAction};
use crate::ui::popover::{display_rgb, menu_picked, open_menu, MenuPlace};
use makepad_widgets_core::property::PropHint;
use makepad_widgets_extras::color::ColorPickerWidgetRefExt;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabColorLinkBase = #(FabColorLink::register_widget(vm))
    mod.widgets.FabColorLink = set_type_default() do mod.widgets.FabColorLinkBase{
        width: Fill
        height: Fit
        color_text: fab.color_text
        color_dim: fab.color_text_dim
        color_border: fab.color_border_light
        color_warning: fab.color_warning
        color_error: fab.color_error
        draw_chip: mod.draw.DrawFabChip{}
        draw_name: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_ui }
        }
        draw_small: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_warn: mod.draw.DrawText{
            color: fab.color_editor
            ink_centered: true
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
        Editor := View{
            width: Fill height: Fit flow: Down spacing: 6
            padding: Inset{left: 0 right: 0 top: 2 bottom: 8}
            title := mod.widgets.FabLabelSmall{ width: Fill text: "" }
            picker := mod.widgets.ColorPicker{ width: Fill popover: false with_alpha: false }
            buttons := View{
                width: Fill height: Fit flow: Right spacing: 6
                material := mod.widgets.FabButton{ text: "Use the design colour" }
                Filler{}
                done := mod.widgets.FabButton{ text: "Done" }
            }
        }
    }

    mod.widgets.FabColorMapBase = #(FabColorMap::register_widget(vm))
    mod.widgets.FabColorMap = set_type_default() do mod.widgets.FabColorMapBase{
        width: Fill
        height: Fit
        draw_small: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        Row := mod.widgets.FabColorLink{}
    }
}

const ROW_H: f64 = 36.0;
const SWATCH: f64 = 20.0;
const WARN: f64 = 16.0;
/// The slot dropdown: the spool's swatch, the tray and the material, the chevron.
const SLOT_W: f64 = 112.0;
/// The room a row keeps for the colour's name beside its swatch. A row narrower than that
/// with its slot beside it puts the slot on a line of its own under the name.
const NAME_MIN: f64 = 48.0;
/// From the row's left edge to the name: the swatch and the gaps round it.
const NAME_X: f64 = 4.0 + SWATCH + 6.0;
/// A line of the note under a row (why an automatic pick moved).
const NOTE_LINE: f64 = 14.0;

/// What a Part panel row's tray pick prints from the tray picked: these parts (`paint`
/// empty), or those painted materials (their colour values) of the one part. The same edit as
/// the part menu's Material flyout: in the colour already there, or a new one saved there.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PartPick {
    pub objects: Vec<String>,
    pub paint: Vec<[f32; 3]>,
}

/// What a place in a row is (for the press and the tooltip).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spot {
    Swatch,
    Mapping,
    Tray,
    /// The ≠ between the design and the slot: the spool's colour is not the design's.
    Differs,
    Warn,
}

#[derive(Script, Widget)]
pub struct FabColorLink {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_name: DrawText,
    #[live]
    draw_small: DrawText,
    #[live]
    draw_warn: DrawText,
    #[live]
    color_text: Vec4f,
    #[live]
    color_dim: Vec4f,
    #[live]
    color_border: Vec4f,
    #[live]
    color_warning: Vec4f,
    #[live]
    color_error: Vec4f,
    /// The published mapping row supplied by the colour map or selected Part panel.
    #[rust]
    pub row: Option<Row>,
    /// The part's own colour (its `color:`), first in the Part panel's list.
    #[rust]
    pub base: bool,
    /// In the Part panel: what a tray pick prints from that tray (the part, or its painted
    /// materials). `None` (the Print tab's list): a pick maps the colour itself there.
    #[rust]
    pub part: Option<PartPick>,
    #[rust]
    template: Option<ScriptObjectRef>,
    #[rust]
    editor: Option<WidgetRef>,
    /// The colour whose design colour the editor changes.
    #[rust]
    editing: Option<String>,
    /// The colour whose tray menu is up.
    #[rust]
    menu_for: Option<String>,
    /// The tray whose contents menu is up (a right-click on the slot while the printer does
    /// not report).
    #[rust]
    tray_editing: Option<super::colours::Position>,
    /// Each drawn spot: its rect, row (colour name) and kind.
    #[rust]
    spots: Vec<(Rect, String, Spot)>,
    #[rust]
    hint: PropHint,
    #[rust]
    area: Area,
    #[rust]
    text: String,
}

impl ScriptHook for FabColorLink {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if kv.key.as_id() == Some(live_id!(Editor)) {
                            if let Some(t) = kv.value.as_object() {
                                self.template = Some(vm.new_object_ref(t));
                            }
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            self.editor = None;
        }
    }
}

pub(super) fn to_srgb(c: [f32; 3]) -> Vec4f {
    let [r, g, b] = display_rgb(c);
    vec4(r, g, b, 1.0)
}

/// Keep compact row labels on one line without changing the row's geometry.
pub(super) fn ellipsis(cx: &mut Cx2d, draw: &DrawText, text: &str, room: f64) -> String {
    let width = |cx: &mut Cx2d, s: &str| draw.prepare_single_line_run(cx, s).map(|r| r.width_in_lpxs as f64).unwrap_or(0.0);
    if width(cx, text) <= room {
        return text.to_string();
    }
    if width(cx, "…") > room {
        return String::new();
    }
    let mut out = text.to_string();
    while !out.is_empty() && width(cx, &format!("{out}…")) > room {
        out.pop();
    }
    out.push('…');
    out
}

/// A round mark with a glyph: "!" for a row's warnings, "≠" where the spool's colour differs from the design's.
pub(super) fn warning_mark(cx: &mut Cx2d, chip: &mut DrawFabChip, text: &mut DrawText, rect: Rect, color: Vec4f, glyph: &str) {
    chip.spool = 0.0;
    chip.empty = 0.0;
    chip.radius = (rect.size.y * 0.5) as f32;
    chip.ring_width = 1.0;
    chip.fill = color;
    chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
    chip.draw_abs(cx, rect);
    let size = text.layout(cx, 0.0, 0.0, None, false, Align::default(), glyph).size_in_lpxs;
    text.draw_abs(cx, rect.pos + dvec2((rect.size.x - size.width as f64) * 0.5, (rect.size.y - size.height as f64) * 0.5), glyph);
}

/// `text` in lines that fit `room`, words kept whole (one wider than the room is cut).
pub(super) fn wrapped(cx: &mut Cx2d, draw: &DrawText, text: &str, room: f64) -> Vec<String> {
    let width = |cx: &mut Cx2d, s: &str| draw.prepare_single_line_run(cx, s).map(|r| r.width_in_lpxs as f64).unwrap_or(0.0);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let longer = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if !line.is_empty() && width(cx, &longer) > room {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        } else {
            line = longer;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.into_iter().map(|l| ellipsis(cx, draw, &l, room)).collect()
}

/// How the row has its slot: "saved" (with the project), "auto" (picked, not saved).
fn how_word(row: &Row) -> &'static str {
    match row.how {
        How::Saved => "saved",
        How::Auto => "auto",
        How::None => "",
    }
}

/// Why an automatic pick moved the colour to its nozzle, when it did.
fn moved_note(row: &Row, heads: &[String]) -> Option<String> {
    let mv = row.moved.as_ref().filter(|mv| row.how == How::Auto && !mv.stays)?;
    Some(format!("Picked for the {} nozzle: {}", heads.get(mv.head)?.to_ascii_lowercase(), mv.why()))
}

/// The slot's material line: the tray's material type and the spool's colour ("PLA light grey").
fn material_text(row: &Row) -> String {
    let mut what = Vec::new();
    let kind = row.type_label();
    if !kind.is_empty() && kind != "?" {
        what.push(kind);
    }
    if let Some([r, g, b, _]) = row.spool() {
        what.push(super::colours::colour_word([r, g, b]).into());
    }
    what.join(" ")
}

/// Why a row has its ! mark: each warning, one a line.
fn warnings_text(row: &Row) -> String {
    row.warnings.iter().map(|w| w.text()).collect::<Vec<_>>().join("\n")
}

/// The "base" tag beside the part's own colour, unless its name says so already ("Base").
fn base_tag(name: &str) -> bool {
    !name.split_whitespace().any(|w| w.eq_ignore_ascii_case("base"))
}

fn row_text(row: &Row, heads: &[String]) -> String {
    if row.unassigned() {
        return format!("{}: no tray", row.name);
    }
    let how = how_word(row);
    let how = if how.is_empty() { String::new() } else { format!(", {how}") };
    let warnings = row.warnings.iter().map(|w| format!("; {}", w.text())).collect::<String>();
    format!("{}: design {}, prints on {}{}{how}{warnings}", row.name, hex(row.design), mapping_text(row, heads), if row.mismatch.is_some() { " (differs from the design)" } else { "" })
}

fn to_linear(c: Vec4f) -> [f32; 3] {
    let f = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    [f(c.x), f(c.y), f(c.z)]
}

/// The tooltip of a spot of a row.
fn tip(state: &AppState, row: &Row, spot: Spot) -> String {
    let m = &state.print.colors;
    if row.used && row.unassigned() {
        return super::colours::UNASSIGNED_TIP.into();
    }
    // An automatic pick moved for the print to change filament less says why.
    let moved = row.moved.as_ref().filter(|m| row.how == How::Auto && !m.stays).map(|mv| format!("Picked automatically for the {} nozzle: {}", m.layout.heads[mv.head].to_ascii_lowercase(), mv.why()));
    let pick = if m.layout.live && !state.print.target_printer().is_some_and(|p| p.virtual_printer) { "Press to pick a tray" } else { "Press to pick a tray; right-click to set what its tray holds" };
    match spot {
        Spot::Swatch => [Some(mapping_tip(row, &m.layout.heads)), moved].into_iter().flatten().collect::<Vec<_>>().join("\n"),
        Spot::Mapping => [Some(mapping_tip(row, &m.layout.heads)), moved, Some(pick.into())].into_iter().flatten().collect::<Vec<_>>().join("\n"),
        Spot::Tray => {
            let parts = state.plate.parts.iter().filter(|p| p.print_color == row.name).count();
            let what = if parts > 0 { format!("{parts} part{}", if parts == 1 { "" } else { "s" }) } else if row.used { "paint".into() } else { "nothing on the plate".into() };
            let what = if state.print.unprinted.contains(&row.name) { format!("{what}, but the slice prints nothing in it: paint covers it all") } else { what };
            let how = match row.how {
                How::Saved => format!("Saved for {}", m.layout.printer),
                How::Auto => moved.map_or_else(|| "Picked automatically (pick a tray to keep it)".into(), |m| format!("{m} (pick a tray to keep it)")),
                How::None => String::new(),
            };
            let tray = row.tray.as_ref().map(|t| tray_tip(t, &m.layout)).unwrap_or_default();
            [format!("{}: {what}", row.name), mapping_tip(row, &m.layout.heads), how, tray, pick.into()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
        }
        Spot::Differs => mapping_tip(row, &m.layout.heads),
        Spot::Warn => format!("{}\n{}", warnings_text(row), mapping_tip(row, &m.layout.heads)),
    }
}

impl FabColorLink {
    fn menu_id(&self) -> LiveId {
        LiveId::from_str(&format!("fab_color_tray:{:?}", self.uid))
    }

    fn tray_edit_id(&self) -> LiveId {
        LiveId::from_str(&format!("fab_color_tray_edit:{:?}", self.uid))
    }

    fn editor(&mut self, cx: &mut Cx) -> Option<WidgetRef> {
        if self.editor.is_none() {
            let template = self.template.as_ref()?;
            let value = template.as_object().into();
            let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
            cx.widget_tree_insert_child(self.uid, live_id!(editor), w.clone());
            self.editor = Some(w);
        }
        self.editor.clone()
    }

    fn press(&mut self, cx: &mut Cx, state: &AppState, name: &str, spot: Spot, rect: Rect) {
        let m = &state.print.colors;
        let Some(row) = m.row(name).cloned() else { return };
        match spot {
            Spot::Swatch => {
                if self.editing.as_deref() == Some(name) {
                    self.editing = None;
                } else {
                    self.editing = Some(name.to_string());
                    if let Some(e) = self.editor(cx) {
                        e.label(cx, ids!(title)).set_text(cx, &format!("{}: model colour", row.name));
                        e.color_picker(cx, ids!(picker)).set_color(cx, to_srgb(row.design));
                        e.button(cx, ids!(buttons.material)).set_enabled(cx, row.over.is_some());
                    }
                }
                cx.redraw_area(self.area);
            }
            Spot::Mapping | Spot::Tray | Spot::Differs | Spot::Warn => {
                self.menu_for = Some(row.name.clone());
                open_menu(cx, self.menu_id(), tray_menu(m, row.pos), rect, MenuPlace::Below);
            }
        }
    }
}

impl Widget for FabColorLink {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, self.row.as_ref().map_or("", |r| r.name.as_str()));
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (i, (rect, name, spot)) in self.spots.iter().enumerate().filter(|(_, (_, _, s))| matches!(s, Spot::Swatch | Spot::Tray | Spot::Differs | Spot::Warn)) {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                bounds: makepad_platform::a11y::semantic_rect(*rect),
                actions: a11y_tree::Actions::PRESS,
                ..Default::default()
            };
            let label = match spot {
                Spot::Swatch => format!("{name} design colour"),
                Spot::Differs => format!("{name} differs: its spool's colour is not the design's"),
                Spot::Warn => format!("{name} warning: {}", self.row.as_ref().map(warnings_text).unwrap_or_default().replace('\n', "; ")),
                _ => self.text.clone(),
            };
            makepad_platform::a11y::set_string(&mut node.name, &label);
            out.push((i as u64 + 1, None, node));
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action != Actions::PRESS {
            return ActionResult::Unhandled;
        }
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some((rect, name, spot)) = key.checked_sub(1).and_then(|i| self.spots.get(i as usize)).cloned() else { return ActionResult::Unsupported };
        let Some(state) = scope.data.get::<AppState>() else { return ActionResult::Disabled };
        self.press(cx, state, &name, spot, rect);
        ActionResult::Handled
    }

    fn visit_cancel(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) -> bool {
        if let Some(e) = &self.editor {
            visit(live_id!(editor), e.clone());
        }
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let (Some(e), Some(_)) = (self.editor.clone(), &self.editing) {
            e.handle_event(cx, event, scope);
        }
        let uid = self.uid;
        if let Event::MouseMove(e) = event {
            let over = self.spots.iter().find(|(r, _, spot)| *spot != Spot::Swatch && r.contains(e.abs)).cloned();
            let text = over.as_ref().and_then(|(_, name, spot)| {
                let state = scope.data.get::<AppState>()?;
                let row = state.print.colors.row(name)?;
                Some(tip(state, row, *spot))
            });
            match (over, text) {
                (Some((r, name, spot)), Some(t)) => {
                    let id = LiveId::from_str(&format!("{name}:{spot:?}"));
                    self.hint.hover(cx, uid, Some((id, r, t.as_str())));
                }
                _ => self.hint.clear(cx, uid),
            }
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(_) | Hit::FingerHoverOut(_) => self.hint.clear(cx, uid),
            // An offline or virtual tray's contents can be set by hand.
            Hit::FingerUp(fe) if fe.device.mouse_button().is_some_and(|b| b.is_secondary()) => {
                if !fe.is_over || fe.cancelled {
                    return;
                }
                let Some(state) = scope.data.get::<AppState>() else { return };
                let m = &state.print.colors;
                let tray = self.spots.iter().find(|(r, _, spot)| *spot == Spot::Mapping && r.contains(fe.abs)).and_then(|(_, name, _)| m.row(name)).and_then(|row| row.pos).filter(|_| !m.layout.live || state.print.target_printer().is_some_and(|p| p.virtual_printer)).and_then(|pos| m.layout.tray(pos));
                if let Some(t) = tray {
                    self.tray_editing = Some(t.pos);
                    self.hint.clear(cx, uid);
                    open_menu(cx, self.tray_edit_id(), super::colours::tray_edit_menu(t, &m.layout.heads), Rect { pos: fe.abs, size: dvec2(0.0, 0.0) }, MenuPlace::At);
                }
            }
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled => {
                if let Some((rect, name, spot)) = self.spots.iter().find(|(r, _, _)| r.contains(fe.abs)).cloned() {
                    self.hint.clear(cx, uid);
                    if let Some(state) = scope.data.get::<AppState>() {
                        self.press(cx, state, &name, spot, rect);
                    }
                }
            }
            _ => {}
        }
        let Event::Actions(actions) = event else { return };
        let Some(state) = scope.data.get::<AppState>() else { return };
        if let Some(pick) = menu_picked(actions, self.tray_edit_id()) {
            if let (Some(pos), Some(edit)) = (self.tray_editing.take(), super::colours::tray_edit_pick(pick)) {
                cx.action(super::PrintAction::EditTray(pos, edit));
            }
        }
        if let Some(pick) = menu_picked(actions, self.menu_id()) {
            if let (Some(name), Some(pos)) = (self.menu_for.take(), tray_pick(&state.print.colors, pick)) {
                cx.action(match &self.part {
                    Some(part) => crate::ui::parts::assign_tray(state, part.objects.clone(), pos, part.paint.clone()),
                    None => PartAction::MapColor { color: name, printer: state.print.colors.layout.printer.clone(), at: Some((super::colours::head_word(&state.print.colors.layout.heads, pos.head), pos.tray_text())) },
                });
            }
        }
        if let (Some(e), Some(name)) = (self.editor.clone(), self.editing.clone()) {
            if let Some(c) = e.color_picker(cx, ids!(picker)).ended(actions) {
                let rgb = to_linear(c);
                let same = state.print.colors.row(&name).is_some_and(|r| hex(r.design) == hex(rgb));
                if !same {
                    cx.action(PartAction::SetColorRgb { color: name.clone(), rgb: Some(rgb) });
                }
            }
            if e.button(cx, ids!(buttons.material)).clicked(actions) {
                cx.action(PartAction::SetColorRgb { color: name, rgb: None });
                self.editing = None;
                cx.redraw_area(self.area);
            } else if e.button(cx, ids!(buttons.done)).clicked(actions) {
                self.editing = None;
                cx.redraw_area(self.area);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.spots.clear();
        self.text.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let heads = &state.print.colors.layout.heads;
        let row = self.row.clone();
        if self.editing.as_ref().is_some_and(|n| row.as_ref().is_none_or(|r| r.name != *n)) {
            self.editing = None;
        }
        cx.begin_turtle(walk, Layout::flow_down());
        if let Some(row) = row {
            let line = cx.walk_turtle(Walk { width: Size::fill(), height: Size::Fixed(ROW_H), ..Walk::default() });
            let (x, y) = (line.pos.x, line.pos.y);
            // Too narrow for the name beside the slot (with the mark between them and the warning
            // column after): the slot takes a line of its own under the name, as wide as fits.
            let stacked = line.size.x < NAME_X + NAME_MIN + 2.0 * (WARN + 6.0) + SLOT_W;
            let (rect, slot) = if stacked {
                let under = cx.walk_turtle(Walk { width: Size::fill(), height: Size::Fixed(ROW_H), ..Walk::default() });
                let width = (x + line.size.x - WARN - 6.0 - (x + NAME_X)).clamp(0.0, SLOT_W);
                (Rect { pos: line.pos, size: dvec2(line.size.x, 2.0 * ROW_H) },
                    Rect { pos: dvec2(x + NAME_X, under.pos.y + 2.0), size: dvec2(width, ROW_H - 4.0) })
            } else {
                (line, Rect { pos: dvec2(x + line.size.x - WARN - 6.0 - SLOT_W, y + 2.0), size: dvec2(SLOT_W, ROW_H - 4.0) })
            };
            // The slot's line: the row's own, or the one under it.
            let sy = slot.pos.y - 2.0;
            // A colour the slice prints nothing in shows as one the plate does not use.
            let alpha = if row.used && !state.print.unprinted.contains(&row.name) { 1.0 } else { 0.5 };
            let unassigned = row.used && row.unassigned();
            self.draw_chip.spool = 0.0;
            self.draw_chip.empty = 0.0;
            self.draw_chip.radius = 3.0;
            self.draw_chip.ring_width = 1.5;
            if unassigned {
                self.draw_chip.fill = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.ring = self.color_error;
                self.draw_chip.draw_abs(cx, rect);
            }
            // The design colour.
            let sw = Rect { pos: dvec2(x + 4.0, y + 8.0), size: dvec2(SWATCH, SWATCH) };
            self.draw_chip.ring_width = 1.0;
            self.draw_chip.fill = to_srgb(row.design);
            self.draw_chip.fill.w *= alpha;
            self.draw_chip.ring = if self.editing.is_some() { self.color_text } else { self.color_border };
            self.draw_chip.draw_abs(cx, sw);
            self.spots.push((sw, row.name.clone(), Spot::Swatch));
            // The slot, at the right: a dropdown of the spool's swatch, its tray and material. The
            // warning mark's column is kept on every row, so the slots of a list line up.
            self.draw_chip.fill = vec4(0.0, 0.0, 0.0, 0.0);
            self.draw_chip.ring = self.color_border;
            self.draw_chip.radius = 4.0;
            self.draw_chip.draw_abs(cx, slot);
            self.draw_chip.radius = 3.0;
            // Between the design and the slot: a ≠ mark where the spool's colour is not the
            // design's, else an arrow.
            if row.mismatch.is_some() {
                let mark = Rect { pos: dvec2(slot.pos.x - WARN - 2.0, sy + (ROW_H - WARN) * 0.5), size: dvec2(WARN, WARN) };
                warning_mark(cx, &mut self.draw_chip, &mut self.draw_warn, mark, self.color_warning, "≠");
                self.spots.push((mark, row.name.clone(), Spot::Differs));
            } else {
                self.draw_small.color = self.color_dim;
                self.draw_small.draw_abs(cx, dvec2(slot.pos.x - 14.0, sy + 12.0), "→");
            }
            let spool = Rect { pos: dvec2(slot.pos.x + 4.0, sy + 8.0), size: sw.size };
            self.draw_chip.ring = self.color_error;
            self.draw_chip.ring_width = 1.5;
            // An unassigned tray is a red hollow square, never a grey spool.
            if !row.unassigned() {
                self.draw_chip.spool = 1.0;
                self.draw_chip.empty = if row.tray.as_ref().is_some_and(|t| t.state == TrayState::Empty) || row.spool().is_none() { 1.0 } else { 0.0 };
                if let Some([r, g, b, _]) = row.spool() {
                    self.draw_chip.fill = to_srgb([r, g, b]);
                }
            } else if !row.used {
                self.draw_chip.ring = self.color_border;
            }
            self.draw_chip.draw_abs(cx, spool);
            self.draw_chip.spool = 0.0;
            self.draw_chip.empty = 0.0;
            self.draw_chip.ring_width = 1.0;
            let tx = spool.pos.x + SWATCH + 6.0;
            let chevron_w = 14.0;
            let slot_room = (slot.pos.x + slot.size.x - chevron_w - tx).max(0.0);
            self.draw_name.color = if unassigned { self.color_error } else { self.color_text };
            self.draw_name.color.w *= alpha;
            let tray = match row.pos.filter(|_| !row.unassigned()) {
                Some(pos) => pos.label(heads),
                None => "No tray".into(),
            };
            let tray = ellipsis(cx, &self.draw_name, &tray, slot_room);
            self.draw_name.draw_abs(cx, dvec2(tx, sy + 3.0), &tray);
            self.draw_small.color = if unassigned { self.color_error } else if row.mismatch.is_some() { self.color_warning } else { self.color_dim };
            self.draw_small.color.w *= alpha;
            let material = if row.unassigned() { "pick one".into() } else { material_text(&row) };
            let material = ellipsis(cx, &self.draw_small, &format!("Spool · {material}"), slot_room);
            self.draw_small.draw_abs(cx, dvec2(tx, sy + 20.0), &material);
            self.draw_small.color = self.color_dim;
            self.draw_small.draw_abs(cx, dvec2(slot.pos.x + slot.size.x - chevron_w + 2.0, sy + 11.0), "▾");
            self.spots.push((slot, row.name.clone(), Spot::Mapping));
            // The name (and "base" for the part's own colour), and how the slot was chosen.
            let name_x = x + NAME_X;
            // Beside the slot, up to its mark; above it, up to the warning column.
            let room = (if stacked { x + line.size.x - WARN - 6.0 } else { slot.pos.x - WARN - 6.0 } - name_x).max(0.0);
            let tag = self.base && base_tag(&row.name);
            let base_w = if tag { self.draw_small.prepare_single_line_run(cx, "base").map(|r| r.width_in_lpxs as f64).unwrap_or(0.0) + 6.0 } else { 0.0 };
            self.draw_name.color = self.color_text;
            self.draw_name.color.w *= alpha;
            let name = ellipsis(cx, &self.draw_name, &row.name, (room - base_w).max(0.0));
            self.draw_name.draw_abs(cx, dvec2(name_x, y + 3.0), &name);
            if tag {
                let name_w = self.draw_name.prepare_single_line_run(cx, &name).map(|r| r.width_in_lpxs as f64).unwrap_or(0.0);
                self.draw_small.color = self.color_dim;
                self.draw_small.draw_abs(cx, dvec2(name_x + name_w + 6.0, y + 5.0), "base");
            }
            self.draw_small.color = self.color_dim;
            self.draw_small.color.w *= alpha;
            let how = ellipsis(cx, &self.draw_small, &format!("Model · {}", how_word(&row)), room);
            self.draw_small.draw_abs(cx, dvec2(name_x, y + 20.0), &how);
            if !row.warnings.is_empty() {
                let wr = Rect { pos: dvec2(x + line.size.x - WARN - 4.0, y + (ROW_H - WARN) * 0.5), size: dvec2(WARN, WARN) };
                warning_mark(cx, &mut self.draw_chip, &mut self.draw_warn, wr, self.color_warning, "!");
                self.spots.push((wr, row.name.clone(), Spot::Warn));
            }
            // Why an automatic pick moved it, under the row.
            if let Some(note) = moved_note(&row, heads) {
                let lines = wrapped(cx, &self.draw_small, &note, (x + rect.size.x - 4.0 - name_x).max(0.0));
                let under = cx.walk_turtle(Walk { width: Size::fill(), height: Size::Fixed(lines.len() as f64 * NOTE_LINE + 4.0), ..Walk::default() });
                self.draw_small.color = self.color_dim;
                for (i, line) in lines.iter().enumerate() {
                    self.draw_small.draw_abs(cx, dvec2(name_x, under.pos.y + i as f64 * NOTE_LINE), line);
                }
            }
            self.spots.push((rect, row.name.clone(), Spot::Tray));
            self.text = row_text(&row, heads);
            if self.base {
                self.text.push_str(" (the part's base colour)");
            }
            if self.editing.is_some() {
                if let Some(e) = self.editor.clone() {
                    e.draw_all(cx, scope);
                }
            }
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// The print editor and printer card share a list of the same rows as the Part panel.
#[derive(Script, Widget)]
pub struct FabColorMap {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[live] draw_small: DrawText,
    #[rust] template: Option<ScriptObjectRef>,
    #[rust] rows: Vec<WidgetRef>,
    #[redraw] #[rust] area: Area,
    #[rust] text: String,
}

impl ScriptHook for FabColorMap {
    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if kv.key.as_id() == Some(live_id!(Row)) {
                            if let Some(t) = kv.value.as_object() {
                                self.template = Some(vm.new_object_ref(t));
                            }
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            self.rows.clear();
        }
    }
}

impl Widget for FabColorMap {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Print colours on the printer");
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
    }

    fn visit_cancel(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) -> bool {
        for (i, row) in self.rows.iter().enumerate() {
            visit(LiveId(i as u64 + 1), row.clone());
        }
        true
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for row in &self.rows {
            row.handle_event(cx, event, scope);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.text.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let m = state.print.colors.clone();
        let sliced = |name: &str| state.print.preview.as_ref().filter(|_| !state.print.stale).and_then(|pv| pv.summary.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()));
        let changes = sliced("Head changes").zip(sliced("Filament swaps"));
        let purge = sliced("Purge");
        cx.begin_turtle(walk, Layout::flow_down());
        let head = if m.layout.live {
            format!("Trays of {}", m.layout.printer)
        } else if m.layout.known {
            format!("{} is not reporting: its last known trays", m.layout.printer)
        } else {
            format!("No printer linked: the {}'s trays (unit B left, A right)", m.layout.printer)
        };
        let rect = cx.walk_turtle(Walk { width: Size::fill(), height: Size::Fixed(18.0), ..Walk::default() });
        self.draw_small.draw_abs(cx, rect.pos + dvec2(0.0, 2.0), &head);
        let mut rows: Vec<&Row> = m.rows.iter().filter(|r| r.used).collect();
        rows.extend(m.rows.iter().filter(|r| !r.used && !r.implicit));
        self.rows.truncate(rows.len());
        if rows.is_empty() {
            let rect = cx.walk_turtle(Walk { width: Size::fill(), height: Size::Fixed(20.0), ..Walk::default() });
            self.draw_small.draw_abs(cx, rect.pos + dvec2(0.0, 3.0), "No parts on the plate yet.");
        }
        for (i, row) in rows.into_iter().enumerate() {
            if self.rows.len() <= i {
                let Some(template) = &self.template else { break };
                let value = template.as_object().into();
                let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
                cx.widget_tree_insert_child(self.uid, LiveId(i as u64 + 1), w.clone());
                self.rows.push(w);
            }
            let w = &self.rows[i];
            if let Some(mut link) = w.borrow_mut::<FabColorLink>() {
                link.row = Some(row.clone());
            }
            w.draw_all(cx, scope);
            self.text.push_str(&row_text(row, &m.layout.heads));
            self.text.push('\n');
        }
        let mut foot = Vec::new();
        for head in 0..m.layout.heads.len() {
            let n = m.tools_on(head);
            if n > 0 {
                foot.push(format!("{}: {n} material{}", m.layout.heads[head], if n == 1 { "" } else { "s" }));
            }
        }
        if let Some((heads, swaps)) = changes {
            foot.push(format!("{heads} head changes"));
            foot.push(format!("{swaps} filament swaps"));
        }
        if let Some(purge) = purge {
            foot.push(format!("purge {purge}"));
        }
        if !foot.is_empty() {
            let rect = cx.walk_turtle(Walk { width: Size::fill(), height: Size::Fixed(18.0), ..Walk::default() });
            self.draw_small.draw_abs(cx, rect.pos + dvec2(0.0, 3.0), &foot.join(" · "));
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}
