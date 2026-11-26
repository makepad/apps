//! The CUT page's small controls (design §4.4): a scrub field whose value
//! is shown right-aligned in its unit (timecode, %, dB, L/C/R, px, a track
//! stepper) and that scrubs when dragged and takes typing when clicked; a
//! segmented control; a toggle switch; and a progress bar.

use super::timeline::timecode_full;
use makepad_widgets::badge::advance;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // CUT and DECK share the document bar's geometry and controls.
    mod.widgets.StageDocumentBar = RoundedView{width: Fill height: 40 flow: Right spacing: 8 padding: Inset{left: 12 right: 12} align: Align{y: 0.5} show_bg: true
        draw_bg +: {color: stage.panel border_color: stage.separator border_size: 1 border_radius: 5}}
    mod.widgets.StageDocumentIcon = Icon{icon_walk: Walk{width: 14 height: Fit} draw_icon +: {color: stage.faint}}
    mod.widgets.StageDocumentName = Label{width: Fit max_lines: 1 text: "" draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 10.5}}}
    mod.widgets.StageDocumentMeta = Label{width: Fit height: Fit text: "" draw_text +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}}
    mod.widgets.StageDocumentDot = RoundedView{width: 8 height: 8 show_bg: true draw_bg +: {color: stage.amber border_radius: 4.0}}
    mod.widgets.StageDocumentChip = RoundedView{width: Fit height: 18 flow: Right spacing: 6 padding: Inset{left: 6 right: 8} align: Align{y: 0.5} show_bg: true
        draw_bg +: {color: stage.raised border_radius: 3.0}}
    mod.widgets.StageDocumentTool = StageButton{width: Fit height: 22 margin: 0 padding: Inset{left: 8 right: 8} spacing: 6
        icon_walk: Walk{width: 11 height: 11}
        draw_icon +: {color: stage.label}
        draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    mod.widgets.StageDocumentIconTool = StageIconButton{margin: 0}
    mod.widgets.StagePanelToggle = mod.widgets.StageDocumentIconTool{
        icon_walk: Walk{width: 11 height: Fit}
        animator +: {open: {
            default: @off
            off: AnimatorState{from: {all: Forward{duration: 0.0}} apply: {
                draw_bg: {active: 0.0 color: stage.control color_focus: stage.control color_hover: stage.control_hover color_down: stage.control_down border_color: stage.border}
                draw_icon: {color: stage.label}
            }}
            on: AnimatorState{from: {all: Forward{duration: 0.0}} apply: {
                draw_bg: {active: 1.0 color: stage.accent color_focus: stage.accent color_hover: stage.accent_hover color_down: stage.accent_down border_color: stage.accent}
                draw_icon: {color: stage.on_accent}
            }}
        }}
    }

    set_type_default() do #(DrawEditControl::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            if self.shape > 2.5 {
                // A progress bar: track, then the done part.
                sdf.box(0., 0., w, h, h * 0.5)
                sdf.fill(self.fill)
                sdf.box(0., 0., max(w * self.amount, h), h, h * 0.5)
                sdf.fill(self.edge)
                return sdf.result
            }
            if self.shape > 1.5 {
                // A switch: track and knob.
                sdf.box(0.5, 0.5, w - 1., h - 1., h * 0.5 - 0.5)
                sdf.fill(self.fill)
                sdf.circle(mix(h * 0.5, w - h * 0.5, self.amount), h * 0.5, h * 0.5 - 2.5)
                sdf.fill(self.edge)
                return sdf.result
            }
            sdf.box(0.5, 0.5, w - 1., h - 1., 2.5)
            sdf.fill_keep(self.fill)
            sdf.stroke(self.edge, 1.)
            if self.shape > 0.5 {
                // A hover or drag underline.
                sdf.rect(2., h - 1.5, w - 4., 1.)
                sdf.fill(vec4(self.edge.xyz, self.amount))
            }
            return sdf.result
        }
    }

    mod.widgets.CutScrubBase = #(CutScrub::register_widget(vm))
    mod.widgets.CutScrub = set_type_default() do mod.widgets.CutScrubBase{
        width: Fill height: 22
        draw_value +: {color: stage.text_bright text_style: theme.font_regular{font_size: 9}}
        draw_unit +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}
        input := TextInput{width: Fill height: 22 margin: 0 padding: Inset{left: 6 right: 6 top: 3 bottom: 3}
            draw_text +: {text_style: theme.font_regular{font_size: 9}}}
    }

    mod.widgets.CutSegmentedBase = #(CutSegmented::register_widget(vm))
    mod.widgets.CutSegmented = set_type_default() do mod.widgets.CutSegmentedBase{
        width: Fill height: 22
        draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 9}}
    }

    mod.widgets.CutSwitchBase = #(CutSwitch::register_widget(vm))
    mod.widgets.CutSwitch = set_type_default() do mod.widgets.CutSwitchBase{
        width: Fill height: 22
        draw_text +: {color: stage.label text_style: theme.font_regular{font_size: 9}}
    }

    mod.widgets.CutRenderButtonBase = #(CutRenderButton::register_widget(vm))
    mod.widgets.CutRenderButton = set_type_default() do mod.widgets.CutRenderButtonBase{
        width: 150 height: 24 idle_text: "Render"
        draw_text +: {color: stage.on_accent text_style: theme.font_bold{font_size: 9}}
        icon_render +: {svg: crate_resource("self:resources/icons/edit/render.svg")}
        icon_check +: {svg: crate_resource("self:resources/icons/check.svg")}
        icon_alert +: {svg: crate_resource("self:resources/icons/alert.svg")}
    }

    mod.widgets.CutFormatTileBase = #(CutFormatTile::register_widget(vm))
    mod.widgets.CutFormatTile = set_type_default() do mod.widgets.CutFormatTileBase{
        width: 128 height: 88 outline: 40.0
        draw_text +: {color: stage.text_hi text_style: theme.font_bold{font_size: 9}}
        draw_detail +: {color: stage.muted text_style: theme.font_regular{font_size: 8}}
    }

    mod.widgets.CutMeterBase = #(CutMeter::register_widget(vm))
    mod.widgets.CutMeter = set_type_default() do mod.widgets.CutMeterBase{
        width: 12 height: 28
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawEditControl {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    pub fill: Vec4f,
    #[live]
    pub edge: Vec4f,
    /// 0 a field, 1 a field with an underline, 2 a switch, 3 a progress bar.
    #[live]
    pub shape: f32,
    #[live]
    pub amount: f32,
}

/// How a scrub field shows and reads its value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Format {
    /// Seconds as `HH:MM:SS:FF` at `fps`.
    Time { fps: f64 },
    /// 0..1 as a percentage.
    #[default]
    Percent,
    /// A linear gain as dB (0 dB = 1.0; 0 is −∞).
    Decibel,
    /// −1..1 as L50 · C · R50.
    Pan,
    /// Pixels.
    Pixels,
    /// Beats per minute.
    Bpm,
    /// A track index as `‹ V2 ›`, with its lane letter.
    Track { letter: char },
    /// An effect dial −1..1: below 0 leaves the effect's own value (Auto).
    Dial,
}

impl Format {
    /// The value's text and its unit.
    pub fn show(self, value: f64) -> (String, &'static str) {
        match self {
            Self::Time { fps } => (timecode_full(value, fps), ""),
            Self::Percent => (format!("{:.0}", value * 100.0), "%"),
            Self::Decibel if value <= 1e-5 => ("−∞".into(), "dB"),
            Self::Decibel => (format!("{:.1}", 20.0 * value.log10()).replace('-', "−"), "dB"),
            Self::Pan if value.abs() < 0.005 => ("C".into(), ""),
            Self::Pan => (format!("{}{:.0}", if value < 0.0 { 'L' } else { 'R' }, value.abs() * 100.0), ""),
            Self::Pixels => (format!("{value:.0}"), "px"),
            Self::Bpm => (format!("{value:.0}"), "bpm"),
            Self::Track { letter } => (format!("{letter}{}", value.round() as i64 + 1), ""),
            Self::Dial if value < 0.0 => ("Auto".into(), ""),
            Self::Dial => (format!("{:.0}", value * 100.0), "%"),
        }
    }

    /// A typed value, relative to `current` where the text says so (`+30`
    /// frames of a time).
    pub fn parse(self, text: &str, current: f64) -> Option<f64> {
        let text = text.trim().replace('−', "-");
        let text = text.trim_end_matches(['%', 's', ' ']).trim_end_matches("dB").trim();
        match self {
            Self::Time { fps } => {
                if let Some(frames) = text.strip_prefix('+') {
                    return frames.parse::<f64>().ok().map(|frames| current + frames / fps);
                }
                if text.contains(':') {
                    let parts = text.split(':').map(|part| part.trim().parse::<f64>().ok()).collect::<Option<Vec<_>>>()?;
                    // Right to left: frames, seconds, minutes, hours.
                    let mut seconds = 0.0;
                    let weights = [1.0 / fps, 1.0, 60.0, 3600.0];
                    for (part, weight) in parts.iter().rev().zip(weights) {
                        seconds += part * weight;
                    }
                    return Some(seconds);
                }
                text.parse().ok()
            }
            Self::Percent | Self::Dial => {
                if matches!(self, Self::Dial) && text.eq_ignore_ascii_case("auto") {
                    return Some(-1.0);
                }
                text.parse::<f64>().ok().map(|value| value / 100.0)
            }
            Self::Decibel => {
                if text.contains('∞') || text.eq_ignore_ascii_case("-inf") {
                    return Some(0.0);
                }
                text.parse::<f64>().ok().map(|db| 10f64.powf(db / 20.0))
            }
            Self::Pan => {
                let upper = text.to_ascii_uppercase();
                if upper == "C" {
                    return Some(0.0);
                }
                let (sign, rest) = match upper.chars().next()? {
                    'L' => (-1.0, &upper[1..]),
                    'R' => (1.0, &upper[1..]),
                    _ => (1.0, upper.as_str()),
                };
                rest.parse::<f64>().ok().map(|value| sign * value / 100.0)
            }
            Self::Pixels | Self::Bpm => text.trim_end_matches("bpm").trim().parse().ok(),
            Self::Track { .. } => text.trim_start_matches(|ch: char| ch.is_alphabetic()).parse::<f64>().ok().map(|n| n - 1.0),
        }
    }

    /// Value change per dragged point.
    fn step(self, range: f64) -> f64 {
        match self {
            Self::Time { fps } => 1.0 / fps,
            Self::Track { .. } => 0.05,
            Self::Pixels => 1.0,
            Self::Bpm => 0.25,
            _ => (range / 200.0).max(0.001),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum ScrubAction {
    #[default]
    None,
    /// Let go, typed or stepped: one cut.
    Commit(f64),
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutScrub {
    #[deref]
    view: View,
    #[live]
    draw_field: DrawEditControl,
    #[live]
    draw_value: DrawText,
    #[live]
    draw_unit: DrawText,
    #[rust]
    format: Format,
    #[rust]
    value: f64,
    #[rust(0.0)]
    min: f64,
    #[rust(1.0)]
    max: f64,
    #[rust]
    keyed: bool,
    #[rust]
    disabled: bool,
    #[rust]
    hover: bool,
    /// A press: where, the value then, and whether it became a drag.
    #[rust]
    press: Option<(f64, f64, bool)>,
    #[rust]
    typing: bool,
    #[rust]
    focus_pending: bool,
    #[rust]
    rect: Rect,
}

impl CutScrub {
    pub fn configure(&mut self, cx: &mut Cx, format: Format, min: f64, max: f64) {
        if self.format != format || self.min != min || self.max != max {
            self.format = format;
            self.min = min;
            self.max = max;
            self.redraw(cx);
        }
    }

    pub fn set_value(&mut self, cx: &mut Cx, value: f64) {
        if self.press.is_none() && !self.typing && self.value != value {
            self.value = value;
            self.redraw(cx);
        }
    }

    pub fn set_keyed(&mut self, cx: &mut Cx, keyed: bool) {
        if self.keyed != keyed {
            self.keyed = keyed;
            self.redraw(cx);
        }
    }

    pub fn set_disabled(&mut self, cx: &mut Cx, disabled: bool) {
        if self.disabled != disabled {
            self.disabled = disabled;
            self.redraw(cx);
        }
    }

    fn clamp(&self, value: f64) -> f64 {
        value.clamp(self.min, self.max)
    }

    fn end_typing(&mut self, cx: &mut Cx, commit: Option<String>) {
        self.typing = false;
        if let Some(text) = commit {
            match self.format.parse(&text, self.value) {
                Some(value) => {
                    self.value = self.clamp(value);
                    cx.widget_action(self.widget_uid(), ScrubAction::Commit(self.value));
                }
                None => log!("edit: `{text}` is not a value here"),
            }
        }
        self.redraw(cx);
    }
}

impl Widget for CutScrub {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.typing {
            self.view.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                let input = self.view.text_input(cx, ids!(input));
                if let Some((text, _)) = input.returned(actions) {
                    self.end_typing(cx, Some(text));
                } else if input.escaped(actions) || input.key_focus_lost(actions) {
                    self.end_typing(cx, None);
                }
            }
            return;
        }
        if self.disabled {
            return;
        }
        let uid = self.widget_uid();
        let track = matches!(self.format, Format::Track { .. });
        match event.hits(cx, self.draw_field.area()) {
            Hit::FingerHoverIn(_) => {
                self.hover = true;
                cx.set_cursor(if track { MouseCursor::Hand } else { MouseCursor::EwResize });
                self.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = false;
                self.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                self.press = Some((fe.abs.x, self.value, false));
                self.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                let Some((x, start, dragging)) = self.press else { return };
                let dx = fe.abs.x - x;
                if !dragging && dx.abs() < 3.0 {
                    return;
                }
                let scale = if fe.modifiers.shift { 0.1 } else if fe.modifiers.logo || fe.modifiers.control { 10.0 } else { 1.0 };
                let value = self.clamp(start + dx * self.format.step(self.max - self.min) * scale);
                self.press = Some((x, start, true));
                self.value = value;
                self.redraw(cx);
            }
            Hit::FingerUp(fe) => {
                let Some((_, _, dragging)) = self.press.take() else { return };
                if fe.cancelled {
                    self.redraw(cx);
                    return;
                }
                if dragging {
                    cx.widget_action(uid, ScrubAction::Commit(self.value));
                } else if track {
                    // The stepper's arrows.
                    let left = fe.abs.x < self.rect.pos.x + self.rect.size.x * 0.5;
                    self.value = self.clamp(self.value.round() + if left { -1.0 } else { 1.0 });
                    cx.widget_action(uid, ScrubAction::Commit(self.value));
                } else {
                    // A click types.
                    self.typing = true;
                    let input = self.view.text_input(cx, ids!(input));
                    input.set_text(cx, &self.format.show(self.value).0);
                    // Focused once it is drawn and has an area.
                    self.focus_pending = true;
                }
                self.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.peek_walk_turtle(walk);
        self.rect = rect;
        let dragging = self.press.is_some_and(|press| press.2);
        let (fill, edge) = if dragging {
            (p.accent_wash, p.accent)
        } else if self.typing {
            (p.well, p.cursor)
        } else if self.hover {
            (p.well, p.border_hi)
        } else {
            (p.well, p.line)
        };
        self.draw_field.fill = Vec4f::from_u32(fill);
        self.draw_field.edge = Vec4f::from_u32(edge);
        self.draw_field.shape = if self.hover && !dragging { 1.0 } else { 0.0 };
        self.draw_field.amount = 0.4;
        if self.typing {
            let step = self.view.draw_walk(cx, scope, walk);
            if std::mem::take(&mut self.focus_pending) {
                let input = self.view.text_input(cx, ids!(input));
                input.take_key_focus(cx);
                if let Some(mut inner) = input.borrow_mut() {
                    inner.select_all(cx);
                };
            }
            return step;
        }
        self.draw_field.draw_walk(cx, walk);
        let (text, unit) = self.format.show(self.value);
        let ink = if self.disabled { p.dim } else if dragging { p.text_max } else if self.keyed { p.accent_soft } else { p.text_bright };
        self.draw_value.color = Vec4f::from_u32(ink);
        let y = rect.pos.y + (rect.size.y - 12.0) * 0.5;
        if let Format::Track { .. } = self.format {
            self.draw_unit.color = Vec4f::from_u32(if self.disabled { p.dim } else { p.label });
            self.draw_unit.draw_abs(cx, dvec2(rect.pos.x + 8.0, y), "‹");
            self.draw_unit.draw_abs(cx, dvec2(rect.pos.x + rect.size.x - 14.0, y), "›");
            let w = advance(&self.draw_value, cx, &text);
            self.draw_value.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - w) * 0.5, y), &text);
            return DrawStep::done();
        }
        let unit_w = if unit.is_empty() { 0.0 } else { advance(&self.draw_unit, cx, unit) + 3.0 };
        let text_w = advance(&self.draw_value, cx, &text);
        let right = rect.pos.x + rect.size.x - 8.0;
        self.draw_value.draw_abs(cx, dvec2(right - unit_w - text_w, y), &text);
        if !unit.is_empty() {
            self.draw_unit.color = Vec4f::from_u32(p.muted);
            self.draw_unit.draw_abs(cx, dvec2(right - unit_w + 3.0, y + 1.0), unit);
        }
        DrawStep::done()
    }
}

#[derive(Clone, Debug, Default)]
pub enum SegmentAction {
    #[default]
    None,
    Selected(usize),
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutSegmented {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_field: DrawEditControl,
    #[live]
    draw_seg: DrawEditControl,
    #[live]
    draw_text: DrawText,
    #[live]
    labels: Vec<String>,
    #[rust]
    active: usize,
    #[rust]
    hover: Option<usize>,
    #[rust]
    rect: Rect,
    /// Each segment's share of the width, from its label's (so a long
    /// label is never squeezed over its neighbours).
    #[rust]
    shares: Vec<f64>,
}

impl CutSegmented {
    pub fn set_active(&mut self, cx: &mut Cx, active: usize) {
        if self.active != active {
            self.active = active;
            self.redraw(cx);
        }
    }

    #[allow(dead_code)]
    pub fn active(&self) -> usize {
        self.active
    }

    fn at(&self, abs: Vec2d) -> Option<usize> {
        if abs.x < self.rect.pos.x {
            return None;
        }
        let mut x = self.rect.pos.x;
        for (i, share) in self.shares.iter().enumerate() {
            x += share * self.rect.size.x;
            if abs.x < x {
                return Some(i);
            }
        }
        None
    }
}

impl Widget for CutSegmented {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // Where it is now: a Fill before it in its row moves it after it
        // was walked, so the walked rect can be stale.
        self.rect = self.draw_field.area().rect(cx);
        match event.hits(cx, self.draw_field.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hover = self.at(fe.abs);
                cx.set_cursor(MouseCursor::Hand);
                if hover != self.hover {
                    self.hover = hover;
                    self.redraw(cx);
                }
            }
            Hit::FingerHoverOut(_) => {
                self.hover = None;
                self.redraw(cx);
            }
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled => {
                if let Some(i) = self.at(fe.abs) {
                    self.active = i;
                    cx.widget_action(self.widget_uid(), SegmentAction::Selected(i));
                    self.redraw(cx);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        self.draw_field.fill = Vec4f::from_u32(p.well);
        self.draw_field.edge = Vec4f::from_u32(p.line);
        self.draw_field.shape = 0.0;
        self.draw_field.draw_abs(cx, rect);
        // Equal segments when every label fits one; otherwise each takes
        // its label's width plus an even share of what is left.
        let labels = self.labels.clone();
        let n = labels.len().max(1) as f64;
        let widths: Vec<f64> = labels.iter().map(|label| advance(&self.draw_text, cx, label) + 12.0).collect();
        let widest = widths.iter().cloned().fold(0.0, f64::max);
        self.shares = if widest * n <= rect.size.x {
            vec![1.0 / n; labels.len()]
        } else {
            let total: f64 = widths.iter().sum();
            let spare = (rect.size.x - total).max(0.0) / n;
            widths.iter().map(|w| (w + spare) / (total + spare * n)).collect()
        };
        let mut x = rect.pos.x;
        for (i, label) in labels.iter().enumerate() {
            let w = self.shares[i] * rect.size.x;
            let seg = Rect { pos: dvec2(x + 1.0, rect.pos.y + 1.0), size: dvec2(w - 2.0, rect.size.y - 2.0) };
            x += w;
            let active = i == self.active;
            let hot = self.hover == Some(i);
            let look = p.pick(active, hot, p.well);
            if active || hot {
                self.draw_seg.fill = Vec4f::from_u32(look.fill);
                self.draw_seg.edge = Vec4f::from_u32(if active { look.edge } else { look.fill });
                self.draw_seg.shape = 0.0;
                self.draw_seg.draw_abs(cx, seg);
            }
            self.draw_text.color = Vec4f::from_u32(look.ink);
            let tw = advance(&self.draw_text, cx, label);
            self.draw_text.draw_abs(cx, dvec2(seg.pos.x + (seg.size.x - tw) * 0.5, seg.pos.y + (seg.size.y - 12.0) * 0.5), label);
        }
        DrawStep::done()
    }
}

#[derive(Clone, Debug, Default)]
pub enum SwitchAction {
    #[default]
    None,
    Toggled(bool),
}

#[derive(Script, ScriptHook, Widget)]
pub struct CutSwitch {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_switch: DrawEditControl,
    #[live]
    draw_text: DrawText,
    #[live]
    text: String,
    #[rust]
    on: bool,
    #[rust]
    disabled: bool,
}

impl CutSwitch {
    pub fn set_on(&mut self, cx: &mut Cx, on: bool) {
        if self.on != on {
            self.on = on;
            self.redraw(cx);
        }
    }

    pub fn set_disabled(&mut self, cx: &mut Cx, disabled: bool) {
        if self.disabled != disabled {
            self.disabled = disabled;
            self.redraw(cx);
        }
    }

    pub fn set_text(&mut self, cx: &mut Cx, text: &str) {
        if self.text != text {
            self.text = text.to_string();
            self.redraw(cx);
        }
    }
}

impl Widget for CutSwitch {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.disabled {
            return;
        }
        match event.hits(cx, self.draw_switch.area()) {
            Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled => {
                self.on = !self.on;
                cx.widget_action(self.widget_uid(), SwitchAction::Toggled(self.on));
                self.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.walk_turtle(walk);
        let knob = Rect { pos: dvec2(rect.pos.x, rect.pos.y + (rect.size.y - 16.0) * 0.5), size: dvec2(28.0, 16.0) };
        self.draw_switch.fill = Vec4f::from_u32(if self.on { p.accent } else { p.control });
        self.draw_switch.edge = Vec4f::from_u32(if self.disabled { p.dim } else { p.text_max });
        self.draw_switch.shape = 2.0;
        self.draw_switch.amount = if self.on { 1.0 } else { 0.0 };
        // The whole row answers the pointer.
        self.draw_switch.draw_abs(cx, knob);
        self.draw_text.color = Vec4f::from_u32(if self.disabled { p.dim } else { p.label });
        let text = self.text.clone();
        self.draw_text.draw_abs(cx, dvec2(knob.pos.x + 36.0, rect.pos.y + (rect.size.y - 12.0) * 0.5), &text);
        DrawStep::done()
    }
}

/// The Render button's look (design §3.2).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RenderLook {
    /// No render, or the saved cut changed: "Render…".
    #[default]
    Idle,
    /// Rendering: done so far, and the label.
    Progress(f64, String),
    Failed,
    /// Nothing to render (no cut open).
    Disabled,
}

#[derive(Clone, Debug, Default)]
pub enum RenderButtonAction {
    #[default]
    None,
    Clicked,
}

/// The page's one primary button: accent-filled at rest, a progress face
/// while rendering (hover: "Cancel render"), and outlined when a render
/// exists (fresh `live`, stale `amber`) or failed (`danger`).
#[derive(Script, ScriptHook, Widget)]
pub struct CutRenderButton {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_face: DrawEditControl,
    #[live]
    draw_fill: DrawEditControl,
    #[live]
    draw_text: DrawText,
    #[live]
    icon_render: DrawSvg,
    #[live]
    icon_check: DrawSvg,
    #[live]
    icon_alert: DrawSvg,
    /// The label at rest.
    #[live]
    idle_text: String,
    #[rust]
    look: RenderLook,
    #[rust]
    hover: bool,
    #[rust]
    down: bool,
}

impl CutRenderButton {
    pub fn set_look(&mut self, cx: &mut Cx, look: RenderLook) {
        if self.look != look {
            self.look = look;
            self.redraw(cx);
        }
    }
}

impl Widget for CutRenderButton {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if self.look == RenderLook::Disabled {
            return;
        }
        match event.hits(cx, self.draw_face.area()) {
            Hit::FingerHoverIn(_) => {
                self.hover = true;
                cx.set_cursor(MouseCursor::Hand);
                self.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = false;
                self.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                self.down = true;
                self.redraw(cx);
            }
            Hit::FingerUp(fe) => {
                self.down = false;
                if fe.is_over && !fe.cancelled {
                    cx.widget_action(self.widget_uid(), RenderButtonAction::Clicked);
                }
                self.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.walk_turtle(walk);
        let (face, edge, ink, label) = match &self.look {
            RenderLook::Idle => {
                let face = if self.down { p.accent_down } else if self.hover { p.accent_hover } else { p.accent };
                (face, face, p.on_accent, self.idle_text.clone())
            }
            RenderLook::Progress(_, label) => {
                // "Rendering 25%", then "· 0:21 left" when it fits (a tab
                // separates the optional tail).
                let text = match label.split_once('\t') {
                    _ if self.hover => "Cancel render".to_string(),
                    Some((head, tail)) => {
                        let long = format!("{head} · {tail}");
                        if advance(&self.draw_text, cx, &long) <= rect.size.x - 38.0 { long } else { head.to_string() }
                    }
                    None => label.clone(),
                };
                (p.control, p.border_hi, p.text_hi, text)
            }
            RenderLook::Failed => (if self.hover { p.control_hover } else { p.control }, p.danger, p.label, "Render failed".into()),
            RenderLook::Disabled => (p.control, p.control, p.dim, self.idle_text.clone()),
        };
        self.draw_face.fill = Vec4f::from_u32(face);
        self.draw_face.edge = Vec4f::from_u32(edge);
        self.draw_face.shape = 0.0;
        self.draw_face.draw_abs(cx, rect);
        if let RenderLook::Progress(amount, _) = &self.look {
            self.draw_fill.fill = Vec4f::from_u32(p.accent);
            self.draw_fill.edge = Vec4f::from_u32(p.accent);
            self.draw_fill.shape = 0.0;
            self.draw_fill.draw_abs(cx, Rect { pos: rect.pos, size: dvec2(rect.size.x * amount.clamp(0.0, 1.0), rect.size.y) });
        }
        let (icon, glyph_ink) = match &self.look {
            RenderLook::Failed => (&mut self.icon_alert, p.danger),
            _ => (&mut self.icon_render, ink),
        };
        icon.color = Vec4f::from_u32(glyph_ink);
        icon.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + 10.0, rect.pos.y + (rect.size.y - 12.0) * 0.5), size: dvec2(12.0, 12.0) });
        let text_x = rect.pos.x + 28.0;
        self.draw_text.color = Vec4f::from_u32(ink);
        self.draw_text.draw_abs(cx, dvec2(text_x, rect.pos.y + (rect.size.y - 13.0) * 0.5), &label);
        DrawStep::done()
    }
}

#[derive(Clone, Debug, Default)]
pub enum TileAction {
    #[default]
    None,
    Clicked,
}

/// A format tile (design §4.6, §4.7): the aspect as an outline, its name
/// and size.
#[derive(Script, ScriptHook, Widget)]
pub struct CutFormatTile {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_face: DrawEditControl,
    #[live]
    draw_outline: DrawEditControl,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_detail: DrawText,
    /// The outline's height.
    #[live]
    outline: f64,
    #[live]
    aspect: Vec2f,
    #[live]
    label: String,
    #[live]
    detail: String,
    #[rust]
    hover: bool,
}

impl Widget for CutFormatTile {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.draw_face.area()) {
            Hit::FingerHoverIn(_) => {
                self.hover = true;
                cx.set_cursor(MouseCursor::Hand);
                self.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.hover = false;
                self.redraw(cx);
            }
            Hit::FingerUp(fe) if fe.is_over && !fe.cancelled => cx.widget_action(self.widget_uid(), TileAction::Clicked),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.walk_turtle(walk);
        let face = if self.hover { p.control_hover } else { p.raised };
        self.draw_face.fill = Vec4f::from_u32(face);
        self.draw_face.edge = Vec4f::from_u32(face);
        self.draw_face.shape = 0.0;
        self.draw_face.draw_abs(cx, rect);
        let aspect = (self.aspect.x.max(1.0) / self.aspect.y.max(1.0)) as f64;
        // The outline fits an `outline` square.
        let (w, h) = if aspect >= 1.0 { (self.outline, self.outline / aspect) } else { (self.outline * aspect, self.outline) };
        let top = rect.pos.y + 10.0 + (self.outline - h) * 0.5;
        self.draw_outline.fill = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_outline.edge = Vec4f::from_u32(p.label);
        self.draw_outline.shape = 0.0;
        self.draw_outline.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + (rect.size.x - w) * 0.5, top), size: dvec2(w, h) });
        let label = self.label.clone();
        let lw = advance(&self.draw_text, cx, &label);
        let y = rect.pos.y + 18.0 + self.outline;
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - lw) * 0.5, y), &label);
        let detail = self.detail.clone();
        let dw = advance(&self.draw_detail, cx, &detail);
        self.draw_detail.draw_abs(cx, dvec2(rect.pos.x + (rect.size.x - dw) * 0.5, y + 16.0), &detail);
        DrawStep::done()
    }
}

/// A stereo level meter (design §3.4): two bars, `live` up to −12 dBFS,
/// `amber` to −3, `hot` above; dim when idle.
#[derive(Script, ScriptHook, Widget)]
pub struct CutMeter {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bar: DrawEditControl,
    #[rust]
    levels: [f64; 2],
}

impl CutMeter {
    /// Linear levels, left and right.
    pub fn set_levels(&mut self, cx: &mut Cx, levels: [f64; 2]) {
        if self.levels != levels {
            self.levels = levels;
            self.redraw(cx);
        }
    }
}

/// A level's place on the meter, 0..1 over −48..0 dBFS.
pub fn meter_fraction(level: f64) -> f64 {
    if level <= 0.0 { 0.0 } else { ((20.0 * level.log10() + 48.0) / 48.0).clamp(0.0, 1.0) }
}

impl Widget for CutMeter {
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let rect = cx.walk_turtle(walk);
        let w = ((rect.size.x - 2.0) * 0.5).floor();
        for (i, level) in self.levels.iter().enumerate() {
            let bar = Rect { pos: dvec2(rect.pos.x + i as f64 * (w + 2.0), rect.pos.y), size: dvec2(w, rect.size.y) };
            self.draw_bar.fill = Vec4f::from_u32(p.groove);
            self.draw_bar.edge = self.draw_bar.fill;
            self.draw_bar.shape = 0.0;
            self.draw_bar.draw_abs(cx, bar);
            let f = meter_fraction(*level);
            // Bands: live to −12, amber to −3, hot above.
            for (from, to, color) in [(0.0, 0.75, p.live), (0.75, 0.9375, p.amber), (0.9375, 1.0, p.hot)] {
                let top = f.min(to);
                if top > from {
                    self.draw_bar.fill = Vec4f::from_u32(color);
                    self.draw_bar.edge = self.draw_bar.fill;
                    let (y0, y1) = (bar.pos.y + bar.size.y * (1.0 - top), bar.pos.y + bar.size.y * (1.0 - from));
                    self.draw_bar.draw_abs(cx, Rect { pos: dvec2(bar.pos.x, y0), size: dvec2(bar.size.x, y1 - y0) });
                }
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::Format;

    #[test]
    fn values_show_in_their_units_and_parse_back() {
        assert_eq!(Format::Time { fps: 60.0 }.show(1.5), ("00:00:01:30".to_string(), ""));
        assert_eq!(Format::Time { fps: 60.0 }.parse("01:12", 0.0), Some(1.2));
        assert_eq!(Format::Time { fps: 60.0 }.parse("+30", 1.0), Some(1.5));
        assert_eq!(Format::Percent.show(0.85), ("85".to_string(), "%"));
        assert_eq!(Format::Decibel.show(1.0), ("0.0".to_string(), "dB"));
        assert_eq!(Format::Decibel.show(0.0).0, "−∞");
        assert!((Format::Decibel.parse("-6", 1.0).unwrap() - 0.501).abs() < 0.001);
        assert_eq!(Format::Pan.show(-0.5).0, "L50");
        assert_eq!(Format::Pan.parse("r25", 0.0), Some(0.25));
        assert_eq!(Format::Track { letter: 'V' }.show(1.0).0, "V2");
        assert_eq!(Format::Dial.show(-1.0).0, "Auto");
        assert_eq!(super::meter_fraction(1.0), 1.0);
        assert_eq!(super::meter_fraction(0.0), 0.0);
    }
}
