//! One probe identity on a lane, wire or carried preview.

use crate::instruments::look::InstrumentLook;
use makepad_widgets_core::makepad_draw::{shader::draw_text::{FontFamily, TextOverflow}, text::layouter::LaidoutText};
use makepad_widgets_core::*;
use std::{collections::HashMap, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Script, ScriptHook)]
#[repr(u32)]
pub enum TagKind { #[pick] Volt = 1, Amp = 2, Power = 3 }

impl TagKind {
    pub fn from_unit(unit: &str) -> Self {
        match unit { "A" => Self::Amp, "W" => Self::Power, _ => Self::Volt }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Script, ScriptHook)]
#[repr(u32)]
pub enum TagState { #[pick] Rest = 1, Hover = 2, Carried = 3, Ghost = 4 }

#[derive(Clone, Copy, Debug, PartialEq, Script, ScriptHook)]
#[repr(u32)]
pub enum LaneSide { #[pick] Sim = 1, Real = 2, Replay = 3 }

impl From<fab_elec_bench::source::Side> for LaneSide {
    fn from(side: fab_elec_bench::source::Side) -> Self {
        use fab_elec_bench::source::Side;
        match side { Side::Sim => Self::Sim, Side::Real => Self::Real, Side::Replay => Self::Replay }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagStyle {
    pub hue: Vec4f,
    pub kind: TagKind,
    pub side: LaneSide,
    pub state: TagState,
}

/// Measured text sizes in logical pixels.
#[derive(Clone, Copy, Debug)]
pub struct TagMetrics { pub title: DVec2, pub pin: DVec2 }

/// Rectangles relative to the tag's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagLayout {
    pub size: DVec2,
    pub glyph: Rect,
    pub title: Rect,
    pub pin: Rect,
}

const PAD: f64 = 4.0;
const GAP: f64 = 5.0;
const CLOCK: f64 = 12.0;

/// Pin space yields first; state never participates in layout.
pub fn layout(title: &str, pin: &str, metrics: TagMetrics, width: f64, kind: TagKind) -> TagLayout {
    let glyph_width = match kind { TagKind::Amp => 12.0, _ => 10.0 };
    let title_width = if title.is_empty() { 0.0 } else { metrics.title.x.max(0.0) };
    let pin_width = if pin.is_empty() || pin == title { 0.0 } else { metrics.pin.x.max(0.0) };
    let height = 22.0f64.max(metrics.title.y.max(metrics.pin.y) + 6.0);
    let natural = PAD * 2.0 + glyph_width + GAP + title_width + if pin_width > 0.0 { GAP + pin_width } else { 0.0 } + CLOCK;
    let width = width.max(0.0).min(natural);
    let left = PAD.min(width);
    let glyph = Rect { pos: dvec2(left, (height - glyph_width) * 0.5), size: dvec2(glyph_width.min((width - left - PAD).max(0.0)), glyph_width) };
    let start = (PAD + glyph_width + GAP).min(width);
    let room = (width - start - PAD - CLOCK).max(0.0);
    let title_width = title_width.min(room);
    let pin_start = (start + title_width + if pin_width > 0.0 { GAP } else { 0.0 }).min(width);
    let pin_width = pin_width.min((room - title_width - GAP).max(0.0));
    let text_rect = |x, width, h: f64| Rect { pos: dvec2(x, (height - h.max(0.0).min(height)) * 0.5), size: dvec2(width, h.max(0.0).min(height)) };
    TagLayout {
        size: dvec2(width, height), glyph,
        title: text_rect(start, title_width, metrics.title.y),
        pin: text_rect(pin_start, pin_width, metrics.pin.y),
    }
}

/// Borrowed labels; no string construction while painting.
pub struct ProbeTag<'a> { pub title: &'a str, pub pin: &'a str }

impl<'a> ProbeTag<'a> {
    pub fn from_label(label: &'a str, pin: &'a str) -> Self {
        let (title, pin) = label.split_once('\n').unwrap_or((label, pin));
        Self { title, pin: if pin == title { "" } else { pin } }
    }

    pub fn layout(&self, metrics: TagMetrics, width: f64, style: TagStyle) -> TagLayout {
        layout(self.title, self.pin, metrics, width, style.kind)
    }

    pub fn measure(&self, cx: &mut Cx2d, shader: &mut DrawProbeTag, text: &DrawText) -> TagMetrics {
        shader.cache.begin(cx, text);
        TagMetrics { title: shader.cache.size(cx, text, self.title), pin: shader.cache.size(cx, text, self.pin) }
    }

    pub fn draw(&self, cx: &mut Cx2d, shader: &mut DrawProbeTag, text: &mut DrawText, rect: Rect, style: TagStyle) {
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 { return; }
        let metrics = self.measure(cx, shader, text);
        let layout = self.layout(metrics, rect.size.x, style);
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        let canvas = look.canvas.canvas;
        let ink = if style.side == LaneSide::Sim { style.hue } else { vec4(canvas[0], canvas[1], canvas[2], style.hue.w) };
        shader.color = style.hue;
        shader.ink = ink;
        shader.kind = style.kind;
        shader.side = style.side;
        shader.state = style.state;
        shader.identity = 1.0;
        let shadow = if style.state == TagState::Carried { 6.0 } else { 0.0 };
        shader.body = vec4(shadow as f32, shadow as f32, rect.size.x as f32, rect.size.y as f32);
        shader.draw_abs(cx, Rect { pos: rect.pos - dvec2(shadow, shadow), size: rect.size + dvec2(shadow * 2.0, shadow * 2.0) });
        // Raw small draws still use the original rounded outline.
        shader.identity = 0.0;
        shader.state = TagState::Rest;
        let old_color = text.color;
        let alpha = if style.state == TagState::Ghost { 0.45 } else { 1.0 };
        cx.push_clip_rect(rect);
        for (label, slot, secondary) in [(self.title, layout.title, false), (self.pin, layout.pin, true)] {
            if label.is_empty() || slot.size.x <= 0.0 { continue; }
            let slot = Rect { pos: rect.pos + slot.pos + dvec2(0.0, (rect.size.y - layout.size.y) * 0.5), size: slot.size };
            let color = if secondary && style.side == LaneSide::Sim {
                let c = look.text_secondary;
                vec4(c[0], c[1], c[2], style.hue.w)
            } else { ink };
            text.color = vec4(color.x, color.y, color.z, color.w * alpha * if secondary { 0.8 } else { 1.0 });
            let laid = shader.cache.fit(cx, text, label, slot.size.x);
            cx.push_clip_rect(slot);
            text.draw_walk_laidout(cx, Walk::abs_rect(slot), &laid);
            cx.pop_clip_rect();
        }
        cx.pop_clip_rect();
        text.color = old_color;
    }
}

/// Place a tag beside a wire point, centred vertically.
pub fn draw_net_tag(cx: &mut Cx2d, shader: &mut DrawProbeTag, text: &mut DrawText, at: DVec2, title: &str, pin: &str, style: TagStyle) {
    let tag = ProbeTag { title, pin };
    let metrics = tag.measure(cx, shader, text);
    let size = tag.layout(metrics, f64::INFINITY, style).size;
    tag.draw(cx, shader, text, Rect { pos: at - dvec2(0.0, size.y * 0.5), size }, style);
}

struct CachedText { full: Rc<LaidoutText>, fitted: Vec<(f64, Rc<LaidoutText>, bool)>, used: bool }

#[derive(Default)]
struct TagCache {
    font: Option<(FontFamily, [u32; 3])>,
    frame: Option<u64>,
    labels: HashMap<String, CachedText>,
}

impl TagCache {
    fn begin(&mut self, cx: &Cx, text: &DrawText) {
        let font = (text.text_style.font_family.clone(), [text.text_style.font_size.to_bits(), text.font_scale.to_bits(), text.text_style.line_spacing.to_bits()]);
        if self.font.as_ref() != Some(&font) { self.labels.clear(); self.font = Some(font); }
        if self.frame != Some(cx.drawing_id()) {
            self.labels.retain(|_, label| {
                let keep = label.used && !label.full.has_missing_glyph;
                label.used = false;
                label.fitted.retain_mut(|(_, laid, used)| { let keep = *used && !laid.has_missing_glyph; *used = false; keep });
                keep
            });
            self.frame = Some(cx.drawing_id());
        }
    }

    fn get(&mut self, cx: &mut Cx2d, text: &DrawText, label: &str) -> &mut CachedText {
        if !self.labels.contains_key(label) {
            self.labels.insert(label.into(), CachedText { full: text.layout(cx, 0.0, 0.0, None, false, Align::default(), label), fitted: Vec::new(), used: true });
        }
        let cached = self.labels.get_mut(label).unwrap();
        cached.used = true;
        cached
    }

    fn size(&mut self, cx: &mut Cx2d, text: &DrawText, label: &str) -> DVec2 {
        if label.is_empty() { return dvec2(0.0, 0.0); }
        let size = self.get(cx, text, label).full.size_in_lpxs;
        dvec2(f64::from(size.width * text.font_scale), f64::from(size.height * text.font_scale))
    }

    fn fit(&mut self, cx: &mut Cx2d, text: &mut DrawText, label: &str, width: f64) -> Rc<LaidoutText> {
        let cached = self.get(cx, text, label);
        if f64::from(cached.full.size_in_lpxs.width * text.font_scale) <= width { return cached.full.clone(); }
        if let Some((_, laid, used)) = cached.fitted.iter_mut().find(|(w, _, _)| *w == width) { *used = true; return laid.clone(); }
        let (overflow, lines) = (text.text_overflow, text.max_lines);
        text.text_overflow = TextOverflow::Ellipsis;
        text.max_lines = 1;
        let laid = text.layout(cx, 0.0, 0.0, Some((width / f64::from(text.font_scale)) as f32), false, Align::default(), label);
        text.text_overflow = overflow;
        text.max_lines = lines;
        cached.fitted.push((width, laid.clone(), true));
        laid
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawProbeTag {
    #[rust] cache: TagCache,
    #[deref] draw_super: DrawColor,
    #[live] pub side: LaneSide,
    #[live] pub ink: Vec4f,
    #[live] kind: TagKind,
    #[live] state: TagState,
    #[live] identity: f32,
    #[live] body: Vec4f,
}

script_mod! {
    use mod.prelude.widgets_internal.*

    let LaneSide = #(LaneSide::script_api(vm))
    let TagKind = #(TagKind::script_api(vm))
    let TagState = #(TagState::script_api(vm))
    mod.draw.DrawProbeTag = set_type_default() do #(DrawProbeTag::script_shader(vm)) {
        ..mod.draw.DrawColor
        side: instance(LaneSide.Sim)
        ink: #fff
        kind: instance(TagKind.Volt)
        state: instance(TagState.Rest)
        identity: 0.0
        body: #0000
        pixel: fn() {
            let mut p = self.pos * self.rect_size
            let mut size = self.rect_size
            if self.identity > 0.5 { p -= self.body.xy; size = self.body.zw }
            let sdf = Sdf2d.viewport(p)
            let mut outline = self.color
            let mut dash = 1.0
            let mut alpha = 1.0
            match self.state {
                TagState.Hover => { outline = vec4(mix(self.color.rgb, vec3(1.0), 0.3), self.color.a) }
                TagState.Carried => {
                    sdf.blur = 4.0
                    sdf.box(-3.0, -1.0, size.x + 6.0, size.y + 6.0, 7.0)
                    sdf.fill(vec4(0.0, 0.0, 0.0, 0.25))
                    sdf.blur = 0.00001
                }
                TagState.Ghost => { alpha = 0.45; dash = step(2.5, mod(p.x + p.y, 7.0)) }
                _ => {}
            }
            sdf.box(0.5, 0.5, max(0.0, size.x - 1.0), max(0.0, size.y - 1.0), 4.0)
            if self.identity > 0.5 {
                match self.kind {
                    TagKind.Volt => { sdf.shape = max(sdf.shape, (4.0 - p.x - abs(p.y - size.y * 0.5)) * 0.7071) }
                    _ => {}
                }
            }
            match self.side {
                LaneSide.Real => { sdf.fill_keep(self.color) }
                LaneSide.Replay => { sdf.fill_keep(self.color) }
                _ => {}
            }
            sdf.stroke(vec4(outline.rgb, outline.a * dash), 1.0)
            if self.identity > 0.5 && size.x >= 18.0 {
                match self.kind {
                    TagKind.Amp => { sdf.circle(10.0, size.y * 0.5, 4.0); sdf.stroke(self.ink, 1.2) }
                    TagKind.Power => { sdf.box(6.0, size.y * 0.5 - 3.0, 6.0, 6.0, 0.0); sdf.fill(self.ink) }
                    _ => {}
                }
                match self.side {
                    LaneSide.Replay => {
                        if size.x >= 32.0 {
                            let x = size.x - 9.0
                            let y = size.y * 0.5
                            sdf.circle(x, y, 4.0)
                            sdf.stroke(self.ink, 1.0)
                            sdf.move_to(x, y - 3.0)
                            sdf.line_to(x, y)
                            sdf.line_to(x + 2.5, y + 1.0)
                            sdf.stroke(self.ink, 1.0)
                        }
                    }
                    _ => {}
                }
            }
            return sdf.result * alpha
        }
    }
}
