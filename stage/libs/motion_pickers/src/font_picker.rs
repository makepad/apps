//! A font written in code (`Font{family weight italic axes}`), chosen by
//! eye.
//!
//! The list is every family a document can name (`makepad_doc_fonts`:
//! the bundled ones and those Stage's Resources downloads), each row the
//! host's sample text (the words being edited) drawn in that family's own
//! face, as the picture draws it. A search narrows it by name, alias or
//! what the family is for. A family that is not installed shows where to
//! get it and cannot be chosen (a document naming it draws in the default
//! face until it is installed).
//!
//! Under the list, the chosen family's own controls: a variable family's
//! axes as sliders over the ranges its file declares (weight, width, slant,
//! italic, optical size); a static family's faces as chips (each weight it
//! has, and its italics).
//!
//! The picker writes nothing. Resting on a row only marks it (the picture
//! does not follow the pointer across the list, so what is chosen stays to
//! be looked at); a click, a chip or a slider let go says
//! [`FontPickAction::Commit`]; a slider says [`FontPickAction::Preview`]
//! while it moves.

use makepad_doc_fonts::{self as fonts, Family, FontFileRole};
use makepad_value_pickers::panel::control::{DrawPanelShape, R_CONTROL};
use makepad_widgets::badge::advance;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    let AxisName = Label{width: 52 text: "" draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 8}}}
    let AxisValue = Label{width: 40 text: "" draw_text +: {color: stage.text_bright text_style: theme.font_regular{font_size: 8}}}
    let AxisRow = View{width: Fill height: Fit flow: Right spacing: 6 align: Align{y: 0.5} visible: false}

    mod.widgets.FontFamilyListBase = #(FontFamilyList::register_widget(vm))
    /** The families, each row its sample in the family's own face. */
    mod.widgets.FontFamilyList = set_type_default() do mod.widgets.FontFamilyListBase{
        width: Fill height: 172
        draw_bg +: {color: #x00000000}
        draw_name +: {color: stage.label text_style: theme.font_regular{font_size: 8}}
        draw_badge +: {color: stage.muted text_style: theme.font_regular{font_size: 7}}
        draw_sample +: {color: stage.text_hi}
    }

    mod.widgets.FontFaceChipsBase = #(FontFaceChips::register_widget(vm))
    /** A static family's faces (weights and italics) as chips. */
    mod.widgets.FontFaceChips = set_type_default() do mod.widgets.FontFaceChipsBase{
        width: Fill height: Fit
        draw_label +: {color: stage.text text_style: theme.font_regular{font_size: 8}}
    }

    mod.widgets.FontPickerBase = #(FontPicker::register_widget(vm))

    /** A font written in code, chosen by eye: the families in their own
     * faces, a variable font's axes, a static family's faces. */
    mod.widgets.FontPicker = set_type_default() do mod.widgets.FontPickerBase{
        width: Fill height: Fit flow: Down spacing: 6
        search := TextInput{width: Fill height: 22 margin: 0 padding: Inset{left: 8 right: 8 top: 3 bottom: 3} empty_text: "Search families"
            draw_bg +: {color: stage.row color_hover: stage.row color_focus: stage.row border_color: stage.line_faint border_size: 1.0 border_radius: 4.0}
            draw_text +: {text_style: theme.font_regular{font_size: 8.5}}}
        list := mod.widgets.FontFamilyList{}
        status := Label{width: Fill max_lines: 1 text_overflow: Ellipsis text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 7.5}}}
        faces := mod.widgets.FontFaceChips{}
        axes := View{width: Fill height: Fit flow: Down spacing: 0
            ax_wght := AxisRow{AxisName{text: "Weight"} slider := StageHSlider{height: 20} value := AxisValue{}}
            ax_wdth := AxisRow{AxisName{text: "Width"} slider := StageHSlider{height: 20} value := AxisValue{}}
            ax_slnt := AxisRow{AxisName{text: "Slant"} slider := StageHSlider{height: 20} value := AxisValue{}}
            ax_ital := AxisRow{AxisName{text: "Italic"} slider := StageHSlider{height: 20} value := AxisValue{}}
            ax_opsz := AxisRow{AxisName{text: "Optical"} slider := StageHSlider{height: 20} value := AxisValue{}}
        }
    }
}

/// A face as a document writes it: `Font{family weight italic axes}`.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FontChoice {
    pub family: String,
    pub weight: f64,
    pub italic: bool,
    /// Variable axes other than the weight (`wdth`, `slnt`, `opsz`, …).
    pub axes: Vec<(String, f64)>,
}

impl FontChoice {
    pub fn axis(&self, tag: &str) -> Option<f64> {
        self.axes.iter().find(|(t, _)| t == tag).map(|(_, v)| *v)
    }

    fn set_axis(&mut self, tag: &str, value: f64) {
        match self.axes.iter_mut().find(|(t, _)| t == tag) {
            Some(slot) => slot.1 = value,
            None => self.axes.push((tag.to_string(), value)),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum FontPickAction {
    #[default]
    None,
    /// A slider moves: show this face, do not write it.
    Preview(FontChoice),
    /// Write this face.
    Commit(FontChoice),
}

/// A variable axis of a family's file: its tag and range.
#[derive(Clone, Debug, PartialEq)]
pub struct AxisRange {
    pub tag: String,
    pub min: f64,
    pub default: f64,
    pub max: f64,
}

/// What the picker knows of a family.
#[derive(Clone, Debug)]
pub struct FamilyInfo {
    pub family: &'static Family,
    pub installed: bool,
    /// Its file's variable axes (empty for a static family, or one not
    /// installed).
    pub axes: Vec<AxisRange>,
    /// A static family's faces: weight and italic.
    pub faces: Vec<(f64, bool)>,
}

impl FamilyInfo {
    fn of(family: &'static Family) -> Self {
        let installed = fonts::installed(family);
        let mut faces: Vec<(f64, bool)> = family.files.iter().filter_map(|f| match f.role {
            FontFileRole::Face { weight: [lo, hi], italic } if lo == hi => Some((lo, italic)),
            _ => None,
        }).collect();
        faces.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.total_cmp(&b.0)));
        faces.dedup();
        // The axes a variable file declares (read once, from the file).
        let axes = if installed && family.files.iter().any(fonts::variable) {
            fonts::pick(family, 400.0, false)
                .and_then(fonts::local_path)
                .and_then(|path| fonts::read_font(&path).ok())
                .and_then(|bytes| makepad_typography::font::OutlineFont::new(bytes, 0).ok())
                .map(|font| font.axes().into_iter().map(|a| AxisRange { tag: a.tag, min: a.min as f64, default: a.default as f64, max: a.max as f64 }).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self { family, installed, axes, faces }
    }

    /// The weight range it can draw.
    fn weight_range(&self) -> (f64, f64) {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for f in self.family.files {
            if let FontFileRole::Face { weight: [a, b], .. } = f.role {
                lo = lo.min(a);
                hi = hi.max(b);
            }
        }
        if lo > hi { (400.0, 400.0) } else { (lo, hi) }
    }

    fn has_italic(&self) -> bool {
        self.family.files.iter().any(|f| matches!(f.role, FontFileRole::Face { italic: true, .. }))
    }

    /// `choice` carried over to this family: its weight within the
    /// family's range (a static family's nearest face), its italic if the
    /// family has one, and the axes the family's file has.
    pub fn adapt(&self, choice: &FontChoice) -> FontChoice {
        let (lo, hi) = self.weight_range();
        let italic = choice.italic && self.has_italic();
        let weight = if self.faces.is_empty() {
            choice.weight.clamp(lo, hi)
        } else {
            self.faces.iter().filter(|f| f.1 == italic).map(|f| f.0).min_by(|a, b| (a - choice.weight).abs().total_cmp(&(b - choice.weight).abs())).unwrap_or(choice.weight)
        };
        let axes = choice.axes.iter().filter(|(t, _)| self.axes.iter().any(|a| a.tag == *t)).map(|(t, v)| {
            let a = self.axes.iter().find(|a| a.tag == *t).expect("filtered");
            (t.clone(), v.clamp(a.min, a.max))
        }).collect();
        FontChoice { family: self.family.name.to_string(), weight, italic, axes }
    }
}

/// A weight's usual name.
fn weight_name(weight: f64) -> &'static str {
    match weight.round() as i32 {
        ..=149 => "Thin",
        150..=249 => "ExtraLight",
        250..=349 => "Light",
        350..=449 => "Regular",
        450..=549 => "Medium",
        550..=649 => "SemiBold",
        650..=749 => "Bold",
        750..=849 => "ExtraBold",
        _ => "Black",
    }
}

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut c = Vec4f::from_u32(value);
    c.w *= alpha;
    c
}

fn fit(draw: &DrawText, cx: &mut Cx2d, text: &str, width: f64) -> String {
    if advance(draw, cx, text) <= width {
        return text.to_string();
    }
    let mut out: String = text.to_string();
    while !out.is_empty() && advance(draw, cx, &format!("{out}…")) > width {
        out.pop();
    }
    format!("{out}…")
}

// ---------------------------------------------------------------- the list

const ROW: f64 = 26.0;

/// The families as rows: name and where it comes from above, the sample in
/// its face below.
#[derive(Script, ScriptHook, Widget)]
pub struct FontFamilyList {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    /// Under the list (what the pointer hits).
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_row: DrawPanelShape,
    #[live]
    draw_name: DrawText,
    #[live]
    draw_badge: DrawText,
    #[live]
    draw_sample: DrawText,
    /// Indices into the picker's families, as filtered.
    #[rust]
    rows: Vec<usize>,
    #[rust]
    families: Vec<FamilyInfo>,
    #[rust]
    sample: String,
    /// The chosen family's index into `families`.
    #[rust]
    selected: Option<usize>,
    #[rust]
    hot: Option<usize>,
    #[rust]
    scroll: f64,
    #[rust]
    rect: Rect,
    /// Each drawn row's rect and family.
    #[rust]
    spots: Vec<(Rect, usize)>,
    /// The family to scroll into view at the next draw.
    #[rust]
    reveal: bool,
}

/// What the list says to its picker.
#[derive(Clone, Debug, Default)]
enum ListAction {
    #[default]
    None,
    Click(usize),
}

impl Widget for FontFamilyList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        let at = |list: &Self, abs: DVec2| list.spots.iter().find(|(r, _)| r.contains(abs)).map(|(_, i)| *i);
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerScroll(fs) => {
                let content = self.rows.len() as f64 * ROW;
                self.scroll = (self.scroll + fs.scroll.y).clamp(0.0, (content - self.rect.size.y).max(0.0));
                self.draw_bg.redraw(cx);
            }
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hot = at(self, fe.abs);
                cx.set_cursor(if hot.is_some_and(|i| self.families[i].installed) { MouseCursor::Hand } else { MouseCursor::Default });
                if hot != self.hot {
                    self.hot = hot;
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hot.take().is_some() {
                    self.draw_bg.redraw(cx);
                }
            }
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() => {
                if let Some(i) = at(self, fe.abs) {
                    cx.widget_action(uid, ListAction::Click(i));
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        let p = makepad_look::palette::get(cx);
        self.draw_bg.color = color(0, 0.0);
        self.draw_bg.draw_abs(cx, rect);
        self.spots.clear();
        if self.reveal {
            self.reveal = false;
            if let Some(k) = self.selected.and_then(|s| self.rows.iter().position(|&i| i == s)) {
                let top = k as f64 * ROW;
                if top < self.scroll || top + ROW > self.scroll + rect.size.y {
                    self.scroll = (top - rect.size.y * 0.4).max(0.0);
                }
            }
        }
        let content = self.rows.len() as f64 * ROW;
        self.scroll = self.scroll.clamp(0.0, (content - rect.size.y).max(0.0));
        cx.push_clip_rect(rect);
        let first = (self.scroll / ROW).floor() as usize;
        let shown = (rect.size.y / ROW).ceil() as usize + 1;
        let rows: Vec<usize> = self.rows.iter().copied().skip(first).take(shown).collect();
        for (k, i) in rows.into_iter().enumerate() {
            let y = rect.pos.y + (first + k) as f64 * ROW - self.scroll;
            let r = Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, ROW) };
            let info = self.families[i].clone();
            let chosen = self.selected == Some(i);
            // One line per family: its name in its own face (at its regular
            // weight), the chosen one washed in the accent; a family not
            // installed in plain type with where to get it.
            if chosen || self.hot == Some(i) {
                let wash = if chosen { color(p.accent_wash, 1.0) } else { color(p.control_hover, 0.7) };
                self.draw_row.fill_round(cx, Rect { pos: r.pos + dvec2(2.0, 1.0), size: r.size - dvec2(4.0, 2.0) }, wash, R_CONTROL);
            }
            let badge = if info.installed { String::new() } else { "get it in Resources".to_string() };
            let badge_w = if badge.is_empty() { 0.0 } else { advance(&self.draw_badge, cx, &badge) + 10.0 };
            if !badge.is_empty() {
                self.draw_badge.color = color(p.warm, 0.8);
                self.draw_badge.draw_abs(cx, dvec2(r.pos.x + r.size.x - badge_w, y + 9.0), &badge);
            }
            let regular = info.faces.iter().filter(|f| !f.1).map(|f| f.0).min_by(|a, b| (a - 400.0).abs().total_cmp(&(b - 400.0).abs())).unwrap_or(400.0);
            let face = if info.installed { makepad_motion_draw::face_style(cx, info.family.name, regular, false, &[]) } else { None };
            match face {
                Some(mut style) => {
                    style.font_size = 10.5;
                    self.draw_sample.text_style = style;
                    self.draw_sample.color = color(if chosen { p.text_max } else { p.text_hi }, 1.0);
                }
                None => {
                    self.draw_sample.text_style = self.draw_name.text_style.clone();
                    self.draw_sample.text_style.font_size = 8.5;
                    self.draw_sample.color = color(p.muted, 1.0);
                }
            }
            // The family's name, or the words being edited when the host
            // gave some (a word or two says more about a face than a letter).
            let own = if self.sample.chars().filter(|c| c.is_alphanumeric()).count() >= 3 && info.installed { format!("{} · {}", info.family.name, self.sample) } else { info.family.name.to_string() };
            let name = fit(&self.draw_sample, cx, &own, r.size.x - 20.0 - badge_w);
            self.draw_sample.draw_abs(cx, dvec2(r.pos.x + 10.0, y + 6.0), &name);
            self.spots.push((r, i));
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }
}

// ---------------------------------------------------------------- the faces

/// Chips, one per face of a static family; the current one marked.
#[derive(Script, ScriptHook, Widget)]
pub struct FontFaceChips {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawPanelShape,
    #[live]
    draw_label: DrawText,
    #[rust]
    faces: Vec<(f64, bool)>,
    #[rust]
    current: Option<(f64, bool)>,
    #[rust]
    spots: Vec<(Rect, usize)>,
    #[rust]
    hot: Option<usize>,
    #[rust]
    area: Area,
}

impl Widget for FontFaceChips {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        let at = |w: &Self, abs: DVec2| w.spots.iter().find(|(r, _)| r.contains(abs)).map(|(_, i)| *i);
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hot = at(self, fe.abs);
                cx.set_cursor(if hot.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                if hot != self.hot {
                    self.hot = hot;
                    self.area.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.hot = None;
                self.area.redraw(cx);
            }
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() => {
                if let Some(i) = at(self, fe.abs) {
                    cx.widget_action(uid, ListAction::Click(i));
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.spots.clear();
        if self.faces.is_empty() {
            cx.walk_turtle(Walk::new(Size::fill(), Size::Fixed(0.0)));
            return DrawStep::done();
        }
        let p = makepad_look::palette::get(cx);
        cx.begin_turtle(walk, Layout::flow_down());
        let width = cx.turtle().rect().size.x;
        let origin = cx.turtle().pos();
        let (h, gap) = (20.0, 4.0);
        let labels: Vec<String> = self.faces.iter().map(|(w, it)| format!("{}{}", weight_name(*w), if *it { " Italic" } else { "" })).collect();
        let mut x = 0.0;
        let mut y = 0.0;
        let mut rects = Vec::new();
        for label in &labels {
            let w = advance(&self.draw_label, cx, label) + 14.0;
            if x > 0.0 && x + w > width {
                x = 0.0;
                y += h + gap;
            }
            rects.push(Rect { pos: dvec2(origin.x + x, origin.y + y), size: dvec2(w, h) });
            x += w + gap;
        }
        cx.walk_turtle(Walk::new(Size::fill(), Size::Fixed(y + h)));
        for (i, (r, label)) in rects.iter().zip(&labels).enumerate() {
            let on = self.current == Some(self.faces[i]);
            let look = p.pick(on, self.hot == Some(i), p.raised);
            self.draw_chip.edged_round(cx, *r, color(look.fill, 1.0), color(look.edge, 1.0), 1.0, R_CONTROL);
            self.draw_label.color = color(look.ink, 1.0);
            self.draw_label.draw_abs(cx, dvec2(r.pos.x + 7.0, r.pos.y + 5.0), label);
            self.spots.push((*r, i));
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

// ---------------------------------------------------------------- the picker

/// The variable axes the picker has a slider for, by its row.
const AXIS_ROWS: [(&str, &[LiveId]); 5] = [
    ("wght", ids!(ax_wght)),
    ("wdth", ids!(ax_wdth)),
    ("slnt", ids!(ax_slnt)),
    ("ital", ids!(ax_ital)),
    ("opsz", ids!(ax_opsz)),
];

#[derive(Script, ScriptHook, Widget)]
pub struct FontPicker {
    #[deref]
    view: View,
    #[rust]
    families: Vec<FamilyInfo>,
    #[rust]
    choice: FontChoice,
    /// The families read (their files' axes, once).
    #[rust]
    read: bool,
    /// Each axis row's range, as shown (`wght` is the weight).
    #[rust]
    ranges: Vec<(String, f64, f64)>,
    #[rust]
    filter: String,
}

impl FontPicker {
    fn ensure_families(&mut self) {
        if !self.read {
            self.families = fonts::FAMILIES.iter().map(FamilyInfo::of).collect();
            self.read = true;
        }
    }

    /// Read the families again (one was installed meanwhile).
    pub fn refresh_families(&mut self, cx: &mut Cx) {
        self.read = false;
        self.ensure_families();
        self.show(cx);
    }

    fn info(&self, family: &str) -> Option<usize> {
        let f = fonts::family(family)?;
        self.families.iter().position(|i| i.family.name == f.name)
    }

    /// The face shown (the host's value; nothing is reported), and the
    /// sample the rows draw (the words being edited).
    pub fn set_value(&mut self, cx: &mut Cx, choice: FontChoice, sample: &str) {
        self.ensure_families();
        self.choice = choice;
        let sample: String = sample.trim().chars().take(48).collect();
        if let Some(mut list) = self.view.widget(cx, ids!(list)).borrow_mut::<FontFamilyList>() {
            list.sample = sample;
            list.reveal = true;
        }
        self.show(cx);
    }

    pub fn value(&self) -> &FontChoice {
        &self.choice
    }

    fn filtered(&self) -> Vec<usize> {
        let q = self.filter.trim().to_lowercase();
        (0..self.families.len()).filter(|&i| {
            let f = self.families[i].family;
            q.is_empty() || f.name.to_lowercase().contains(&q) || f.aliases.iter().any(|a| a.to_lowercase().contains(&q)) || f.role.to_lowercase().contains(&q)
        }).collect()
    }

    fn show(&mut self, cx: &mut Cx) {
        let selected = self.info(&self.choice.family);
        let rows = self.filtered();
        let families = self.families.clone();
        if let Some(mut list) = self.view.widget(cx, ids!(list)).borrow_mut::<FontFamilyList>() {
            list.families = families;
            list.rows = rows;
            list.selected = selected;
        }
        self.view.widget(cx, ids!(list)).redraw(cx);
        let info = selected.map(|i| self.families[i].clone());
        let status = match &info {
            // What the family is for (its name and weight are the row's
            // and the list's already).
            Some(info) => info.family.role.to_string(),
            None => format!("“{}” is no font this desk knows", self.choice.family),
        };
        self.view.label(cx, ids!(status)).set_text(cx, &status);
        // A static family: its faces.
        let faces = info.as_ref().map(|i| i.faces.clone()).unwrap_or_default();
        if let Some(mut chips) = self.view.widget(cx, ids!(faces)).borrow_mut::<FontFaceChips>() {
            chips.faces = faces;
            chips.current = Some((self.choice.weight, self.choice.italic));
        }
        self.view.widget(cx, ids!(faces)).redraw(cx);
        // A variable family: its axes over its file's ranges (the weight
        // over the family's range, which its files say).
        self.ranges.clear();
        for (tag, row) in AXIS_ROWS {
            let range = info.as_ref().and_then(|info| {
                if tag == "wght" && info.faces.is_empty() {
                    let (lo, hi) = info.weight_range();
                    return (hi > lo).then_some((lo, hi, self.choice.weight));
                }
                let a = info.axes.iter().find(|a| a.tag == tag && tag != "wght")?;
                (a.max > a.min).then(|| (a.min, a.max, self.choice.axis(tag).unwrap_or(a.default)))
            });
            let row_ref = self.view.widget(cx, row);
            row_ref.set_visible(cx, range.is_some());
            if let Some((lo, hi, value)) = range {
                self.ranges.push((tag.to_string(), lo, hi));
                let t = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
                row_ref.widget(cx, ids!(slider)).as_slider().set_value(cx, t);
                row_ref.widget(cx, ids!(value)).set_text(cx, &axis_text(tag, value));
            }
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let uid = self.widget_uid();
        if let Some(text) = self.view.widget(cx, ids!(search)).as_text_input().changed(actions) {
            self.filter = text;
            if let Some(mut list) = self.view.widget(cx, ids!(list)).borrow_mut::<FontFamilyList>() {
                list.scroll = 0.0;
            }
            self.show(cx);
        }
        let list_uid = self.view.widget(cx, ids!(list)).widget_uid();
        for action in actions.filter_widget_actions_cast::<ListAction>(list_uid) {
            match action {
                ListAction::Click(i) => {
                    let info = self.families[i].clone();
                    if !info.installed {
                        let note = format!("{} is not installed: install it in Stage's Resources (SCORE ⋯ → Resources…), then choose it here.", info.family.name);
                        self.view.label(cx, ids!(status)).set_text(cx, &note);
                        continue;
                    }
                    self.choice = info.adapt(&self.choice);
                    self.show(cx);
                    cx.widget_action(uid, FontPickAction::Commit(self.choice.clone()));
                }
                ListAction::None => {}
            }
        }
        let chips_uid = self.view.widget(cx, ids!(faces)).widget_uid();
        for action in actions.filter_widget_actions_cast::<ListAction>(chips_uid) {
            if let ListAction::Click(i) = action {
                let face = self.view.widget(cx, ids!(faces)).borrow::<FontFaceChips>().and_then(|c| c.faces.get(i).copied());
                if let Some((weight, italic)) = face {
                    self.choice.weight = weight;
                    self.choice.italic = italic;
                    self.show(cx);
                    cx.widget_action(uid, FontPickAction::Commit(self.choice.clone()));
                }
            }
        }
        for (tag, row) in AXIS_ROWS {
            let Some((lo, hi)) = self.ranges.iter().find(|r| r.0 == tag).map(|r| (r.1, r.2)) else { continue };
            let row_ref = self.view.widget(cx, row);
            let slider = row_ref.widget(cx, ids!(slider)).as_slider();
            let moved = slider.slided(actions).map(|t| (t, false)).or_else(|| slider.end_slide(actions).map(|t| (t, true)));
            let Some((t, done)) = moved else { continue };
            let mut value = lo + (hi - lo) * t.clamp(0.0, 1.0);
            // Whole units: a weight of 512.37 is nobody's intent.
            value = if hi - lo >= 20.0 { value.round() } else { (value * 10.0).round() / 10.0 };
            if tag == "wght" {
                self.choice.weight = value;
            } else {
                self.choice.set_axis(tag, value);
            }
            row_ref.widget(cx, ids!(value)).set_text(cx, &axis_text(tag, value));
            if done {
                self.show(cx);
            }
            cx.widget_action(uid, if done { FontPickAction::Commit(self.choice.clone()) } else { FontPickAction::Preview(self.choice.clone()) });
        }
    }
}

fn axis_text(tag: &str, value: f64) -> String {
    let n = if value.fract().abs() < 1e-9 { format!("{}", value as i64) } else { format!("{value:.1}") };
    match tag {
        "slnt" => format!("{n}°"),
        "wdth" => format!("{n}%"),
        "opsz" => format!("{n} pt"),
        _ => n,
    }
}

impl Widget for FontPicker {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            self.handle_actions(cx, actions);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

impl FontPickerRef {
    /// What the picker said in this batch of actions.
    pub fn picked(&self, actions: &Actions) -> Vec<FontPickAction> {
        actions.filter_widget_actions_cast::<FontPickAction>(self.widget_uid()).filter(|a| !matches!(a, FontPickAction::None)).collect()
    }

    pub fn set_value(&self, cx: &mut Cx, choice: FontChoice, sample: &str) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_value(cx, choice, sample);
        }
    }
}
