//! A stub's driven pins. Edits stay on the UI thread; only a pending drag preview asks for a frame.

use super::{
    look::InstrumentLook,
    scope::{div_steps, AnswerWidthClass, LabelCache, ScopeSettings, GAP, PAD},
    sources::{edit, BenchEdit, BenchKey, BenchView},
};
use crate::{live::sim::SimAction, snapshot::Refusal, trace::{cursor::eng, draw::DrawTraceSeg}};
use fab_elec_bench::{bench::Stub, trigger::Window};
use fab_elec_core::drive::{Drive, Pull};
use makepad_widgets_core::*;
use std::collections::{HashMap, HashSet};

const HEAD: f64 = 18.0;
const PICTURE: f64 = 56.0;
const GLYPH: f64 = 22.0;
const EDGE_HIT: f64 = 4.0;
const COMPUTED: &str = "computed in the bench source";
/// The dash length of a computed value's underline.
const DASH: f64 = 2.0;

/// Nominal PWM rise/fall pairs for periods intersecting `[-window.pre, window.post)`.
/// Rises are whole periods from the trigger at zero; a pair may extend outside the window.
/// Duty is clamped to 0..=1. Invalid frequency, duty or window returns no edges.
/// Cost is proportional to the number of periods: drawing must use a band before periods
/// become narrower than four points, rather than calling this for an unresolved waveform.
pub fn pwm_edges(freq: f64, duty: f64, window: Window) -> Vec<(f64, f64)> {
    let span = window.pre + window.post;
    if !freq.is_finite() || freq <= 0.0 || !duty.is_finite()
        || !window.pre.is_finite() || !window.post.is_finite() || !span.is_finite() || span <= 0.0
    {
        return Vec::new();
    }
    let first = (-window.pre * freq).floor();
    let end = (window.post * freq).ceil();
    if !first.is_finite() || !end.is_finite() { return Vec::new(); }
    let duty = duty.clamp(0.0, 1.0);
    (0..(end - first).max(0.0) as usize).map(|i| {
        let rise = (first + i as f64) / freq;
        (rise, rise + duty / freq)
    }).collect()
}

/// Duty implied by a falling edge `t` seconds after its grabbed period's rising edge.
/// Keep that rising edge fixed throughout a drag (including pre-trigger periods), so dragging
/// past either end saturates at the knob's 0..=1 limits instead of wrapping to another period.
/// Invalid frequency or NaN time returns zero.
pub fn duty_from_drag(freq: f64, t: f64) -> f64 {
    if !freq.is_finite() || freq <= 0.0 || t.is_nan() { return 0.0; }
    (freq * t).clamp(0.0, 1.0)
}

/// Frequency implied by a period bracket from `t_rise` to `t`, in trigger-relative seconds.
/// Returns `None` for non-finite endpoints or `t <= t_rise`; otherwise clamps to the knob's
/// 1 Hz..=10 MHz range. The left end uses the same geometry with both times negated.
pub fn freq_from_drag(t_rise: f64, t: f64) -> Option<f64> {
    if !t_rise.is_finite() || !t.is_finite() || t <= t_rise { return None; }
    Some((1.0 / (t - t_rise)).clamp(1.0, 10e6))
}

mod registration {
    use super::*;

    script_mod! {
        use mod.prelude.widgets_internal.*
        use mod.widgets.*

        mod.widgets.DriveLaneBase = #(DriveLane::register_widget(vm))
        mod.widgets.DriveLane = set_type_default() do mod.widgets.DriveLaneBase {
            width: Fill height: Fit
            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 12}}
            draw_label +: {color: theme.color_text text_style: theme.font_regular {font_size: 11}}
            draw_seg: mod.draw.DrawTraceSeg {}
        }
    }
}

/// Registers the drive lane after the trace shaders.
pub fn script_mod(vm: &mut ScriptVm) {
    registration::script_mod(vm);
}

/// The bench wraps these in view actions with its bench name and the card's stub path.
#[derive(Clone, Debug, Default)]
pub(super) enum GeneratorAction {
    SetDrive { pin: String, drive: Drive, field: &'static str, preview: bool },
    CommitPreview,
    #[default]
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Pin,
    Pwm,
    High,
    Low,
    Input,
    Frequency,
    Duty,
    Level,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Part {
    pin: usize,
    kind: Kind,
}

impl Part {
    fn key(self) -> u64 {
        1 + self.pin as u64 * 8 + self.kind as u64
    }

    fn knob(self) -> bool {
        matches!(self.kind, Kind::Frequency | Kind::Duty)
    }
}

struct Control {
    part: Part,
    label: String,
    selected: bool,
}

struct PartRect {
    control: Control,
    rect: Rect,
}

#[derive(Clone, Copy, PartialEq)]
struct PictureDrag {
    plot: Rect,
    window: Window,
    anchor: f64,
    left: bool,
}

impl PictureDrag {
    fn time(self, x: f64) -> f64 {
        (x - self.plot.pos.x) / self.plot.size.x * (self.window.pre + self.window.post) - self.window.pre
    }
}

#[derive(Clone, Copy)]
struct Picture {
    pin: usize,
    plot: Rect,
    window: Window,
    freq: f64,
    duty: f64,
    rise: f64,
}

impl Picture {
    fn x(self, t: f64) -> f64 {
        self.plot.pos.x + (t + self.window.pre) / (self.window.pre + self.window.post) * self.plot.size.x
    }

    fn period_width(self) -> f64 {
        self.plot.size.x / ((self.window.pre + self.window.post) * self.freq)
    }

    fn hit(self, point: DVec2) -> Option<(Kind, PictureDrag)> {
        if self.period_width() < EDGE_HIT { return None; }
        let geometry = PictureDrag { plot: self.plot, window: self.window, anchor: self.rise, left: false };
        let y = self.plot.pos.y + self.plot.size.y + 9.0;
        if (point.y - y).abs() <= 6.0 {
            for left in [false, true] {
                let end = if left { self.rise } else { self.rise + 1.0 / self.freq };
                let x = self.x(end);
                if x >= self.plot.pos.x && x <= self.plot.pos.x + self.plot.size.x && (point.x - x).abs() <= EDGE_HIT {
                    return Some((Kind::Frequency, PictureDrag {
                        anchor: if left { self.rise + 1.0 / self.freq } else { self.rise }, left, ..geometry
                    }));
                }
            }
        }
        if point.y < self.plot.pos.y || point.y > self.plot.pos.y + self.plot.size.y { return None; }
        let cycle = (geometry.time(point.x) * self.freq - self.duty).round();
        let rise = cycle / self.freq;
        let x = self.x(rise + self.duty / self.freq);
        if x >= self.plot.pos.x && x <= self.plot.pos.x + self.plot.size.x && (point.x - x).abs() <= EDGE_HIT {
            return Some((Kind::Duty, PictureDrag { anchor: rise, ..geometry }));
        }
        None
    }
}

struct Drag {
    part: Part,
    grab: f64,
    picture: Option<PictureDrag>,
    freq: f64,
    duty: f64,
    changed: bool,
    released: bool,
}

#[derive(Clone, PartialEq)]
struct Pin {
    pin: String,
    drive: Drive,
    edit: BenchEdit,
}

/// One stub's drive lanes; the host owns the evaluated values, time window and source edit.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct DriveLane {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_shape: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_label: DrawText,
    #[live]
    draw_seg: DrawTraceSeg,
    #[rust]
    text_cache: LabelCache,
    #[rust]
    label_cache: LabelCache,
    #[rust]
    area: Area,
    #[rust]
    drawn_at: DVec2,
    #[rust]
    title: String,
    #[rust]
    stub: String,
    #[rust]
    pins: Vec<Pin>,
    #[rust]
    pwm: HashMap<String, (f64, f64)>,
    #[rust]
    parts: Vec<PartRect>,
    #[rust]
    pictures: Vec<Picture>,
    #[rust]
    expanded: HashMap<String, bool>,
    #[rust(AnswerWidthClass::Narrow)]
    width_class: AnswerWidthClass,
    #[rust(ScopeSettings::default().window())]
    window: Window,
    #[rust]
    hint: PropHint,
    #[rust]
    pointer: Option<DVec2>,
    #[rust]
    focused: Option<Part>,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    pending: bool,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    refusal_seq: Option<u64>,
    #[rust]
    refusal: Option<String>,
    #[rust(true)]
    dark: bool,
}

impl DriveLane {
    /// Show the nominal drive over the bench's shared trigger-relative time window.
    /// The rise at zero and repetitions describe what the source writes, not a measurement.
    /// An unchanged window requests no redraw or frame; invalid windows are ignored.
    pub fn set_window(&mut self, cx: &mut Cx, window: Window) {
        let span = window.pre + window.post;
        if window.pre.is_finite() && window.post.is_finite() && span.is_finite() && span > 0.0 && self.window != window {
            self.window = window;
            self.pictures.clear();
            self.area.redraw(cx);
        }
    }

    /// Keep the active pin's optimistic value across host refreshes. Return whether its drag ended.
    pub(super) fn set_source(&mut self, cx: &mut Cx, source: &BenchView, stub: &Stub, title: String) -> bool {
        let mut pins: Vec<_> = stub.pins.iter().map(|(pin, drive)| Pin {
            pin: pin.clone(), drive: drive.clone(), edit: edit(source, &BenchKey::Stub(stub.path.clone(), pin.clone())).clone(),
        }).collect();
        let active = self.drag.as_ref().and_then(|drag| self.pins.get(drag.part.pin)).cloned();
        let mut active_index = None;
        let mut seen = HashSet::new();
        for (i, pin) in pins.iter_mut().enumerate() {
            seen.insert(pin.pin.clone());
            if let Some(active) = active.as_ref().filter(|active| active.pin == pin.pin && matches!(pin.edit, BenchEdit::Literal)) {
                pin.drive = active.drive.clone();
                active_index = Some(i);
            }
            if let Drive::Pwm { freq, duty } = pin.drive {
                self.pwm.insert(pin.pin.clone(), (freq, duty));
            }
        }
        self.pwm.retain(|pin, _| seen.contains(pin));
        self.expanded.retain(|pin, _| seen.contains(pin));
        let cancelled = if let Some(i) = active_index {
            if let Some(drag) = &mut self.drag { drag.part.pin = i; }
            false
        } else {
            self.cancel_drag(cx)
        };
        if self.title == title && self.stub == stub.path && self.pins == pins {
            return cancelled;
        }
        let reordered = !self.pins.iter().map(|pin| &pin.pin).eq(pins.iter().map(|pin| &pin.pin));
        self.title = title;
        self.stub = stub.path.clone();
        self.pins = pins;
        if reordered {
            self.hint.clear(cx, self.uid);
            self.parts.clear();
            self.pictures.clear();
            self.focused = None;
        }
        cx.widget_tree_mark_dirty(self.uid);
        self.area.redraw(cx);
        cancelled
    }

    pub(super) fn cancel_drag(&mut self, cx: &mut Cx) -> bool {
        self.pending = false;
        if self.drag.take().is_some() {
            self.hint.clear(cx, self.uid);
            self.area.redraw(cx);
            return true;
        }
        false
    }

    pub(super) fn set_refusal(&mut self, cx: &mut Cx, refusal: Option<&Refusal>) {
        let text = match refusal {
            Some(refusal) if refusal.label.strip_prefix(self.stub.as_str()).is_some_and(|rest| rest.starts_with(' ')) => {
                if self.refusal_seq == Some(refusal.seq) { return; }
                self.refusal_seq = Some(refusal.seq);
                Some(refusal.reason.clone())
            }
            Some(_) => return,
            None => None,
        };
        if self.refusal != text {
            self.refusal = text;
            self.area.redraw(cx);
        }
    }

    fn begin_gesture(&mut self, cx: &mut Cx, part: Part) -> bool {
        self.refusal = None;
        self.area.redraw(cx);
        if let BenchEdit::Refused(diag) = &self.pins[part.pin].edit {
            self.refusal = Some(diag.message.clone());
            return false;
        }
        if matches!(self.pins[part.pin].edit, BenchEdit::Derived { .. }) { return false; }
        cx.action(SimAction::BeginGesture);
        true
    }

    fn emit(&self, cx: &mut Cx, part: Part, preview: bool) {
        let pin = &self.pins[part.pin];
        let field = match part.kind {
            Kind::Duty => "duty",
            Kind::Frequency => "frequency",
            _ => "mode",
        };
        cx.widget_action(self.uid, GeneratorAction::SetDrive {
            pin: pin.pin.clone(), drive: pin.drive.clone(), field, preview,
        });
    }

    fn request_frame(&mut self, cx: &mut Cx) {
        if self.pending && !cx.next_frame_is_pending(self.next_frame) {
            self.next_frame = cx.new_next_frame();
        }
    }

    fn finish_drag(&mut self, cx: &mut Cx) {
        if let Some(drag) = self.drag.take() {
            if drag.changed { cx.widget_action(self.uid, GeneratorAction::CommitPreview); }
            self.area.redraw(cx);
        }
    }

    fn replace(&mut self, pin: usize, drive: Drive) -> bool {
        if self.pins[pin].drive == drive {
            return false;
        }
        if let Drive::Pwm { freq, duty } = drive {
            self.pwm.insert(self.pins[pin].pin.clone(), (freq, duty));
        }
        self.pins[pin].drive = drive;
        true
    }

    fn commit(&mut self, cx: &mut Cx, part: Part, drive: Drive) {
        if self.drag.is_some() {
            return;
        }
        if !self.begin_gesture(cx, part) { return; }
        self.replace(part.pin, drive);
        self.emit(cx, part, false);
        cx.widget_tree_mark_dirty(self.uid);
        self.area.redraw(cx);
    }

    fn expanded(&self, pin: usize) -> bool {
        self.width_class != AnswerWidthClass::Narrow && matches!(self.pins[pin].drive, Drive::Pwm { .. })
            && self.expanded.get(&self.pins[pin].pin).copied().unwrap_or(self.width_class == AnswerWidthClass::Wide)
    }

    fn interactive(&self, part: Part) -> bool {
        if part.kind == Kind::Pin {
            return self.width_class != AnswerWidthClass::Narrow && matches!(self.pins[part.pin].drive, Drive::Pwm { .. });
        }
        part.kind != Kind::Level && !matches!(self.pins[part.pin].edit, BenchEdit::Derived { .. })
    }

    fn activate(&mut self, cx: &mut Cx, part: Part) {
        if part.kind == Kind::Pin {
            if self.interactive(part) && self.drag.is_none() {
                self.expanded.insert(self.pins[part.pin].pin.clone(), !self.expanded(part.pin));
                cx.widget_tree_mark_dirty(self.uid);
                self.area.redraw(cx);
            }
            return;
        }
        let drive = match part.kind {
            Kind::Pwm => {
                let (freq, duty) = self.pwm.get(&self.pins[part.pin].pin).copied().unwrap_or((1e3, 0.5));
                Drive::Pwm { freq, duty }
            }
            Kind::High => Drive::Level(true),
            Kind::Low => Drive::Level(false),
            Kind::Input => Drive::Input(Pull::None),
            _ => return,
        };
        self.commit(cx, part, drive);
    }

    fn set_value(&mut self, cx: &mut Cx, part: Part, value: f64) {
        if !value.is_finite() {
            return;
        }
        let Drive::Pwm { mut freq, mut duty } = self.pins[part.pin].drive else { return };
        match part.kind {
            Kind::Frequency => freq = value.clamp(1.0, 10e6),
            Kind::Duty => duty = (value / 100.0).clamp(0.0, 1.0),
            _ => return,
        }
        self.commit(cx, part, Drive::Pwm { freq, duty });
    }

    fn step(&mut self, cx: &mut Cx, part: Part, steps: i32) {
        let Drive::Pwm { freq, duty } = self.pins[part.pin].drive else { return };
        let value = match part.kind {
            Kind::Frequency => div_steps(freq, steps),
            Kind::Duty => duty * 100.0 + f64::from(steps),
            _ => return,
        };
        self.set_value(cx, part, value);
    }

    fn drag_to(&mut self, x: f64, shift: bool) {
        let Some(drag) = &self.drag else { return };
        let dx = x - drag.grab;
        if !dx.is_finite() || (dx == 0.0 && !drag.changed) {
            return;
        }
        let part = drag.part;
        let (mut freq, mut duty) = (drag.freq, drag.duty);
        if let Some(picture) = drag.picture {
            let t = picture.time(x);
            match part.kind {
                Kind::Duty => duty = duty_from_drag(freq, t - picture.anchor),
                Kind::Frequency => {
                    let value = if picture.left { freq_from_drag(-picture.anchor, -t) } else { freq_from_drag(picture.anchor, t) };
                    let Some(value) = value else { return };
                    freq = value;
                }
                _ => return,
            }
        } else {
            match part.kind {
                Kind::Duty => duty = (duty + dx * if shift { 0.00025 } else { 0.0025 }).clamp(0.0, 1.0),
                Kind::Frequency => freq = (freq * 2.0_f64.powf(dx / 120.0)).clamp(1.0, 10e6),
                _ => return,
            }
        }
        if self.replace(part.pin, Drive::Pwm { freq, duty }) {
            self.drag.as_mut().unwrap().changed = true;
            self.pending = true;
        }
    }

    fn picture_at(&self, point: DVec2) -> Option<(Part, PictureDrag)> {
        self.pictures.iter().rev().find_map(|picture| picture.hit(point).map(|(kind, geometry)| (Part { pin: picture.pin, kind }, geometry)))
    }

    fn hand_at(&self, point: DVec2) -> bool {
        self.picture_at(point).map(|(part, _)| part).or_else(|| self.part_at(point)).is_some_and(|part| self.interactive(part))
    }

    fn hover_hint(&mut self, cx: &mut Cx, point: Option<DVec2>) {
        let value = point.and_then(|point| self.parts.iter().find(|p| p.rect.contains(point)
            && p.control.part.kind != Kind::Pin && matches!(self.pins[p.control.part.pin].edit, BenchEdit::Derived { .. })));
        self.hint.hover(cx, self.uid, value.map(|p| (LiveId(p.control.part.key()), p.rect, COMPUTED)));
    }

    fn part_at(&self, point: DVec2) -> Option<Part> {
        self.parts.iter().rev().find(|part| part.rect.contains(point)).map(|part| part.control.part)
    }

    fn part_name(&self, part: Part) -> String {
        let role = match part.kind {
            Kind::Pin => "pin",
            Kind::Pwm => "pwm",
            Kind::High => "high",
            Kind::Low => "low",
            Kind::Input => "input",
            Kind::Frequency => "frequency",
            Kind::Duty => "duty",
            Kind::Level => "level",
        };
        format!("{} {role}", self.pins[part.pin].pin)
    }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        self.draw_shape.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_shape.draw_abs(cx, rect);
    }

    fn text_width(&mut self, cx: &mut Cx2d, text: &str, numeric: bool) -> f64 {
        if numeric { self.text_cache.get(cx, &self.draw_text, text).width } else { self.label_cache.get(cx, &self.draw_label, text).width }
    }

    fn text_box(&mut self, cx: &mut Cx2d, rect: Rect, text: &str, color: [f32; 4], numeric: bool) {
        if rect.size.x <= 0.0 || text.is_empty() {
            return;
        }
        let (draw, cache) = if numeric { (&mut self.draw_text, &mut self.text_cache) } else { (&mut self.draw_label, &mut self.label_cache) };
        let layout = cache.get(cx, draw, text).fit(cx, draw, rect.size.x);
        draw.color = vec4(color[0], color[1], color[2], color[3]);
        cx.push_clip_rect(rect);
        draw.draw_walk_laidout(cx, Walk::abs_rect(rect), &layout);
        cx.pop_clip_rect();
    }

    fn line(&mut self, cx: &mut Cx2d, a: DVec2, b: DVec2, color: [f32; 4], width: f32) {
        // Tight quads keep a row's raster work linear in its width too.
        let pad = f64::from(width) * 0.5 + 1.0;
        let rect = Rect { pos: dvec2(a.x.min(b.x) - pad, a.y.min(b.y) - pad), size: dvec2((b.x - a.x).abs() + pad * 2.0, (b.y - a.y).abs() + pad * 2.0) };
        let (a, b) = (a - rect.pos, b - rect.pos);
        self.draw_seg.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_seg.width = width;
        self.draw_seg.seg = vec4(a.x as f32, a.y as f32, b.x as f32, b.y as f32);
        self.draw_seg.draw_abs(cx, rect);
    }

    fn glyph(&mut self, cx: &mut Cx2d, rect: Rect, kind: Kind, ink: [f32; 4]) {
        let (left, right, top, bottom) = (rect.pos.x + 3.0, rect.pos.x + rect.size.x - 3.0, rect.pos.y + 3.0, rect.pos.y + HEAD - 3.0);
        let mid = (top + bottom) * 0.5;
        match kind {
            Kind::High | Kind::Low => {
                let y = if kind == Kind::High { top } else { bottom };
                self.line(cx, dvec2(left, y), dvec2(right, y), ink, 1.5);
            }
            Kind::Pwm => {
                let rise = left + (right - left) * 0.2;
                let fall = left + (right - left) * 0.65;
                let points = [dvec2(left, bottom), dvec2(rise, bottom), dvec2(rise, top), dvec2(fall, top), dvec2(fall, bottom), dvec2(right, bottom)];
                for pair in points.windows(2) { self.line(cx, pair[0], pair[1], ink, 1.5); }
            }
            Kind::Input => {
                self.line(cx, dvec2(left, mid), dvec2(right - 3.0, mid), ink, 1.5);
                self.line(cx, dvec2(right - 7.0, mid - 3.0), dvec2(right - 3.0, mid), ink, 1.5);
                self.line(cx, dvec2(right - 7.0, mid + 3.0), dvec2(right - 3.0, mid), ink, 1.5);
                self.line(cx, dvec2(right, top), dvec2(right, bottom), ink, 1.5);
            }
            _ => {}
        }
    }

    fn draw_head(&mut self, cx: &mut Cx2d, i: usize, row: Rect) {
        let look = InstrumentLook::for_theme(self.dark);
        let ink = look.channel(i);
        let pin = &self.pins[i];
        let (name, drive, computed) = (pin.pin.clone(), pin.drive.clone(), matches!(pin.edit, BenchEdit::Derived { .. }));
        let narrow = self.width_class == AnswerWidthClass::Narrow;
        let shape_width = (4.0 * GLYPH).min(row.size.x);
        let limit = row.pos.x + row.size.x - shape_width - GAP;
        let title = self.title.clone();
        let pin_width = self.text_width(cx, &name, false);
        let title_width = self.text_width(cx, &title, true);
        let tag_width = (pin_width + title_width + 3.0 * GAP).min((limit - row.pos.x).max(0.0) * if narrow { 0.45 } else { 0.4 });
        let tag = Rect { pos: row.pos, size: dvec2(tag_width, HEAD) };
        let part = Part { pin: i, kind: Kind::Pin };
        let hot = self.pointer.is_some_and(|p| tag.contains(p));
        self.quad(cx, tag, [ink[0], ink[1], ink[2], if hot { 0.18 } else { 0.09 }]);
        let name_width = pin_width.min((tag_width - GAP * 2.0).max(0.0));
        self.text_box(cx, Rect { pos: tag.pos + dvec2(GAP, 1.0), size: dvec2(name_width, HEAD - 1.0) }, &name, ink, false);
        self.text_box(cx, Rect { pos: tag.pos + dvec2(name_width + GAP * 2.0, 2.0), size: dvec2((tag_width - name_width - GAP * 3.0).max(0.0), HEAD - 2.0) }, &title, look.text_secondary, true);
        self.parts.push(PartRect { control: Control { part, label: format!("{} {}", name, title), selected: self.expanded(i) }, rect: tag });
        if self.focused == Some(part) { self.line(cx, tag.pos + dvec2(0.0, HEAD), tag.pos + dvec2(tag_width, HEAD), look.canvas.selection, 1.0); }
        let mut x = row.pos.x + tag_width + PAD;
        if !narrow {
            let width = self.text_width(cx, "drive", false).min((limit - x).max(0.0));
            self.text_box(cx, Rect { pos: dvec2(x, row.pos.y + 1.0), size: dvec2(width, HEAD - 1.0) }, "drive", look.text_secondary, false);
            x += width + PAD;
        }
        let values = match drive {
            Drive::Pwm { freq, duty } => {
                let mut values = vec![(Kind::Frequency, eng(freq, "Hz", 3))];
                if !narrow { values.push((Kind::Duty, format!("{:.1} % high", duty * 100.0))); }
                values
            }
            Drive::Level(high) => vec![(Kind::Level, (if high { "high" } else { "low" }).into())],
            Drive::Input(_) => Vec::new(),
        };
        for (kind, label) in values {
            let part = Part { pin: i, kind };
            let width = self.text_width(cx, &label, true).min((limit - x).max(0.0));
            if width <= 0.0 { break; }
            let rect = Rect { pos: dvec2(x, row.pos.y), size: dvec2(width, HEAD) };
            self.text_box(cx, Rect { pos: rect.pos + dvec2(0.0, 2.0), size: dvec2(width, HEAD - 2.0) }, &label, look.text, true);
            let hot = self.pointer.is_some_and(|p| rect.contains(p)) && self.interactive(part);
            let color = if self.focused == Some(part) { look.canvas.selection } else if hot { ink } else { look.zero_line };
            if computed {
                // A value the bench source computes is underlined with dashes: it is changed there.
                let mut dash = 0.0;
                while dash < width {
                    self.line(cx, rect.pos + dvec2(dash, HEAD), rect.pos + dvec2((dash + DASH).min(width), HEAD), color, 1.0);
                    dash += 2.0 * DASH;
                }
            } else if kind != Kind::Level {
                self.line(cx, rect.pos + dvec2(0.0, HEAD), rect.pos + dvec2(width, HEAD), color, 1.0);
            }
            self.parts.push(PartRect { control: Control { part, label, selected: false }, rect });
            x += width + PAD * 2.0;
        }
        let mut x = row.pos.x + row.size.x - shape_width;
        for (kind, label, selected) in [
            (Kind::High, "high", drive == Drive::Level(true)),
            (Kind::Low, "low", drive == Drive::Level(false)),
            (Kind::Pwm, "pwm", matches!(drive, Drive::Pwm { .. })),
            (Kind::Input, "input", matches!(drive, Drive::Input(_))),
        ] {
            let part = Part { pin: i, kind };
            let rect = Rect { pos: dvec2(x, row.pos.y), size: dvec2(shape_width / 4.0, HEAD) };
            cx.push_clip_rect(rect);
            self.glyph(cx, rect, kind, if selected { ink } else { look.text_tertiary });
            cx.pop_clip_rect();
            if self.focused == Some(part) || self.pointer.is_some_and(|p| rect.contains(p)) {
                self.line(cx, rect.pos + dvec2(2.0, HEAD), rect.pos + dvec2((rect.size.x - 2.0).max(2.0), HEAD), if self.focused == Some(part) { look.canvas.selection } else { look.zero_line }, 1.0);
            }
            self.parts.push(PartRect { control: Control { part, label: label.into(), selected }, rect });
            x += shape_width / 4.0;
        }
    }

    fn draw_picture(&mut self, cx: &mut Cx2d, i: usize, rect: Rect) {
        let Drive::Pwm { freq, duty } = self.pins[i].drive else { return };
        if !freq.is_finite() || freq <= 0.0 || !duty.is_finite() { return; }
        let look = InstrumentLook::for_theme(self.dark);
        let ink = look.channel(i);
        let quiet = [ink[0], ink[1], ink[2], 0.65];
        let plot = Rect { pos: rect.pos + dvec2(0.0, GAP), size: dvec2(rect.size.x, PICTURE - HEAD - GAP * 2.0) };
        let period = 1.0 / freq;
        if !period.is_finite() { return; }
        let middle = (self.window.post - self.window.pre) * 0.5;
        let mut rise = (middle * freq - 0.5).round() / freq;
        // Keep the bracket's anchored end still while its other end rides the pointer.
        if let Some(drag) = self.drag.as_ref().filter(|drag| drag.part.pin == i && drag.part.kind == Kind::Frequency) {
            if let Some(geometry) = drag.picture { rise = if geometry.left { geometry.anchor - period } else { geometry.anchor }; }
        }
        let picture = Picture { pin: i, plot, window: self.window, freq, duty: duty.clamp(0.0, 1.0), rise };
        self.pictures.push(picture);
        cx.push_clip_rect(rect);
        let (left, right, top, bottom) = (plot.pos.x, plot.pos.x + plot.size.x, plot.pos.y, plot.pos.y + plot.size.y);
        self.line(cx, dvec2(left, top), dvec2(right, top), look.zero_line, 1.0);
        self.line(cx, dvec2(left, bottom), dvec2(right, bottom), look.zero_line, 1.0);
        if picture.period_width() < EDGE_HIT {
            self.quad(cx, plot, [ink[0], ink[1], ink[2], picture.duty as f32 * 0.65]);
            let label = eng(freq, "Hz", 3);
            let width = self.text_width(cx, &label, true).min(plot.size.x);
            self.text_box(cx, Rect { pos: dvec2(left + (plot.size.x - width) * 0.5, top + (plot.size.y - HEAD) * 0.5), size: dvec2(width, HEAD) }, &label, look.text, true);
        } else {
            // At least four points per period: at most ceil(width / 4) + 1 periods.
            self.draw_seg.begin_many_instances(cx);
            for (rise, fall) in pwm_edges(freq, picture.duty, self.window) {
                let (a, b, end) = (picture.x(rise), picture.x(fall), picture.x(rise + period));
                if a >= left && a <= right { self.line(cx, dvec2(a, bottom), dvec2(a, top), quiet, 1.5); }
                if b > a && b > left && a < right {
                    self.line(cx, dvec2(a.clamp(left, right), top), dvec2(b.clamp(left, right), top), ink, 1.5);
                }
                if b >= left && b <= right {
                    let hot = self.interactive(Part { pin: i, kind: Kind::Duty }) && self.pointer.is_some_and(|p| (p.x - b).abs() <= EDGE_HIT && p.y >= top && p.y <= bottom);
                    self.line(cx, dvec2(b, top), dvec2(b, bottom), if hot { ink } else { quiet }, 1.5);
                }
                if end > b && end > left && b < right {
                    self.line(cx, dvec2(b.clamp(left, right), bottom), dvec2(end.clamp(left, right), bottom), ink, 1.5);
                }
            }
            let y = bottom + 9.0;
            let (a, b) = (picture.x(rise), picture.x(rise + period));
            self.line(cx, dvec2(a.clamp(left, right), y), dvec2(b.clamp(left, right), y), quiet, 1.0);
            for (x, direction) in [(a, 1.0), (b, -1.0)] {
                if x < left || x > right { continue; }
                let hot = self.interactive(Part { pin: i, kind: Kind::Frequency }) && self.pointer.is_some_and(|p| (p.x - x).abs() <= EDGE_HIT && (p.y - y).abs() <= 6.0);
                let color = if hot { ink } else { quiet };
                let arrow = (picture.period_width() * 0.25).min(4.0);
                self.line(cx, dvec2(x, y - 4.0), dvec2(x, y + 4.0), color, 1.0);
                self.line(cx, dvec2(x, y), dvec2(x + direction * arrow, y - 3.0), color, 1.0);
                self.line(cx, dvec2(x, y), dvec2(x + direction * arrow, y + 3.0), color, 1.0);
            }
            self.draw_seg.end_many_instances(cx);
            let label = eng(period, "s", 3);
            let width = self.text_width(cx, &label, true).min(plot.size.x);
            let x = ((a + b) * 0.5 - width * 0.5).clamp(left, right - width);
            let label_rect = Rect { pos: dvec2(x, y - HEAD * 0.5), size: dvec2(width, HEAD) };
            // A very short bracket keeps both ends visible: its label goes beside it.
            if b - a >= width + PAD * 2.0 {
                self.quad(cx, label_rect, look.canvas.canvas);
                self.text_box(cx, label_rect, &label, look.text_secondary, true);
            } else if b + GAP + width <= right {
                self.text_box(cx, Rect { pos: dvec2(b + GAP, label_rect.pos.y), ..label_rect }, &label, look.text_secondary, true);
            } else if a - GAP - width >= left {
                self.text_box(cx, Rect { pos: dvec2(a - GAP - width, label_rect.pos.y), ..label_rect }, &label, look.text_secondary, true);
            }
        }
        cx.pop_clip_rect();
    }
}

impl WidgetNode for DriveLane {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _point: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for DriveLane {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, mut walk: Walk) -> DrawStep {
        let theme = base_theme(cx);
        self.dark = theme != BaseTheme::Light;
        let look = InstrumentLook::for_theme(self.dark);
        self.text_cache.begin_frame(&self.draw_text, theme);
        self.label_cache.begin_frame(&self.draw_label, theme);
        if walk.width.is_fit() { walk.width = Size::Fixed(600.0); }
        let width = cx.peek_walk_turtle(walk).size.x.max(1.0);
        let class = AnswerWidthClass::from_width(width);
        if self.width_class != class {
            self.width_class = class;
            cx.widget_tree_mark_dirty(self.uid);
        }
        let inner = (width - PAD * 2.0).max(1.0);
        let outputs: Vec<_> = self.pins.iter().enumerate().filter_map(|(i, pin)| (!matches!(pin.drive, Drive::Input(_))).then_some(i)).collect();
        let mut inputs = String::new();
        for pin in &self.pins {
            if let Drive::Input(pull) = pin.drive {
                if inputs.is_empty() { inputs.push_str(&self.title); inputs.push_str(" inputs"); }
                inputs.push(' ');
                inputs.push_str(&pin.pin);
                inputs.push_str(match pull { Pull::Up => " ↑", Pull::Down => " ↓", Pull::None => "" });
            }
        }
        let refusal = self.refusal.as_ref().map(|text| self.draw_label.layout(cx, 0.0, 0.0, Some((inner / f64::from(self.draw_label.font_scale)) as f32), true, Align::default(), text));
        let refusal_h = refusal.as_ref().map_or(0.0, |layout| f64::from(layout.size_in_lpxs.height * self.draw_label.font_scale) + GAP);
        if walk.height.is_fit() {
            let rows: f64 = outputs.iter().map(|&i| HEAD + GAP + if self.expanded(i) { PICTURE } else { 0.0 }).sum();
            walk.height = Size::Fixed(PAD * 2.0 + rows + refusal_h + if inputs.is_empty() { 0.0 } else { HEAD + GAP });
        }
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        self.drawn_at = view.pos;
        self.parts.clear();
        self.pictures.clear();
        cx.begin_turtle(Walk::abs_rect(view), Layout::flow_overlay());
        cx.push_clip_rect(view);
        self.quad(cx, view, look.canvas.canvas);
        let mut y = view.pos.y + PAD;
        for i in outputs {
            let row = Rect { pos: dvec2(view.pos.x + PAD, y), size: dvec2(inner, HEAD) };
            self.draw_head(cx, i, row);
            y += HEAD;
            if self.expanded(i) {
                self.draw_picture(cx, i, Rect { pos: dvec2(row.pos.x, y), size: dvec2(inner, PICTURE) });
                y += PICTURE;
            }
            y += GAP;
        }
        if let Some(layout) = refusal {
            let warning = look.canvas.warning;
            self.draw_label.color = vec4(warning[0], warning[1], warning[2], warning[3]);
            self.draw_label.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos: dvec2(view.pos.x + PAD, y), size: dvec2(inner, refusal_h) }), &layout);
            y += refusal_h;
        }
        if !inputs.is_empty() {
            self.text_box(cx, Rect { pos: dvec2(view.pos.x + PAD, y + 1.0), size: dvec2(inner, HEAD - 1.0) }, &inputs, look.text_secondary, false);
        }
        self.line(cx, view.pos + dvec2(PAD, view.size.y - 1.0), view.pos + dvec2(width - PAD, view.size.y - 1.0), look.zero_line, 1.0);
        cx.pop_clip_rect();
        cx.end_turtle();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if matches!(event, Event::NextFrame(_)) {
            if self.next_frame.is_event(event).is_some() && std::mem::take(&mut self.pending) {
                if let Some(drag) = &self.drag {
                    self.emit(cx, drag.part, true);
                }
                if self.drag.as_ref().is_some_and(|drag| drag.released) {
                    self.finish_drag(cx);
                }
            }
            return;
        }
        let placed = self.area.rect(cx);
        let shift = placed.pos - self.drawn_at;
        if placed.size.x > 0.0 && shift != dvec2(0.0, 0.0) {
            for part in &mut self.parts { part.rect.pos += shift; }
            for picture in &mut self.pictures { picture.plot.pos += shift; }
            if let Some(geometry) = self.drag.as_mut().and_then(|drag| drag.picture.as_mut()) { geometry.plot.pos += shift; }
            self.drawn_at = placed.pos;
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let before = self.pointer.and_then(|p| self.part_at(p));
                let edge_before = self.pointer.and_then(|p| self.picture_at(p));
                self.pointer = Some(e.abs);
                let hovered = self.part_at(e.abs);
                self.hover_hint(cx, Some(e.abs));
                cx.set_cursor(if self.hand_at(e.abs) || self.drag.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
                if before != hovered || edge_before != self.picture_at(e.abs) { self.area.redraw(cx); }
            }
            Hit::FingerHoverOut(_) => {
                self.hint.clear(cx, self.uid);
                if self.drag.is_none() {
                    self.pointer = None;
                    cx.set_cursor(MouseCursor::Default);
                    self.area.redraw(cx);
                }
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                self.pointer = Some(e.abs);
                self.hint.clear(cx, self.uid);
                let picture = self.picture_at(e.abs);
                if let Some(part) = picture.map(|(part, _)| part).or_else(|| self.part_at(e.abs)) {
                    self.focused = Some(part);
                    cx.set_key_focus_from_user(self.area);
                    self.area.redraw(cx);
                    if !self.interactive(part) { return; }
                    cx.set_cursor(MouseCursor::Hand);
                    if part.knob() {
                        if let Drive::Pwm { freq, duty } = self.pins[part.pin].drive {
                            if self.drag.is_some() || !self.begin_gesture(cx, part) { return; }
                            self.drag = Some(Drag { part, grab: e.abs.x, picture: picture.map(|(_, geometry)| geometry), freq, duty, changed: false, released: false });
                            self.area.redraw(cx);
                        }
                    } else {
                        self.activate(cx, part);
                    }
                }
            }
            Hit::FingerMove(e) if self.drag.as_ref().is_some_and(|drag| !drag.released) => {
                self.pointer = Some(e.abs);
                self.drag_to(e.abs.x, e.modifiers.shift);
                self.request_frame(cx);
                cx.set_cursor(MouseCursor::Hand);
                self.area.redraw(cx);
            }
            Hit::FingerUp(e) if self.drag.as_ref().is_some_and(|drag| !drag.released) => {
                self.pointer = Some(e.abs);
                self.drag_to(e.abs.x, e.modifiers.shift);
                if let Some(drag) = &mut self.drag { drag.released = true; }
                if self.pending {
                    // Flush the final value on the next frame before committing the preview.
                    self.request_frame(cx);
                } else {
                    self.finish_drag(cx);
                }
                cx.set_cursor(if self.hand_at(e.abs) { MouseCursor::Hand } else { MouseCursor::Default });
                self.area.redraw(cx);
            }
            Hit::FingerScroll(e) if self.drag.is_none() => {
                self.hint.clear(cx, self.uid);
                let delta = if e.scroll.y != 0.0 { e.scroll.y } else { e.scroll.x };
                if let Some(part) = self.part_at(e.abs).filter(|part| part.knob() && self.interactive(*part) && delta.is_finite() && delta != 0.0) {
                    if let Event::Scroll(scroll) = event {
                        if e.scroll.y != 0.0 { scroll.handled_y.set(true); } else { scroll.handled_x.set(true); }
                    }
                    self.step(cx, part, if delta < 0.0 { 1 } else { -1 });
                }
            }
            Hit::KeyDown(e) => {
                self.hint.clear(cx, self.uid);
                match e.key_code {
                    KeyCode::Tab if !self.parts.is_empty() => {
                        let current = self.focused.and_then(|p| self.parts.iter().position(|r| r.control.part == p));
                        let next = match current {
                            Some(i) if e.modifiers.shift => (i + self.parts.len() - 1) % self.parts.len(),
                            Some(i) => (i + 1) % self.parts.len(),
                            None if e.modifiers.shift => self.parts.len() - 1,
                            None => 0,
                        };
                        self.focused = Some(self.parts[next].control.part);
                        self.area.redraw(cx);
                    }
                    KeyCode::ReturnKey | KeyCode::Space => {
                        if let Some(part) = self.focused { self.activate(cx, part); }
                    }
                    KeyCode::ArrowUp | KeyCode::ArrowRight | KeyCode::ArrowDown | KeyCode::ArrowLeft => {
                        if let Some(part) = self.focused.filter(|part| self.interactive(*part)) {
                            self.step(cx, part, if matches!(e.key_code, KeyCode::ArrowUp | KeyCode::ArrowRight) { 1 } else { -1 });
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, &format!("{} drive", self.title));
    }

    fn semantics_lookup(&self, cx: &Cx, lookup: &SemanticLookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let id_key = match lookup {
            SemanticLookup::Terms(q) => q.id_term().filter(|id| cx.a11y.action_owner(*id).uid() == self.uid.0).and_then(|id| cx.a11y.action_part_key(id)),
            SemanticLookup::Key(_) => None,
        };
        if let Some(lookup) = super::semantics::Lookup::new(lookup, id_key) { self.lookup(cx, &lookup, out); }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        let Some(key) = cx.a11y.action_part_key(request.node) else { return a11y_tree::ActionResult::Unhandled };
        self.lookup_action(cx, key, request)
    }

}

impl DriveLane {
    fn lookup_key(&self, part: Part) -> u64 {
        // Pin identity survives a source reorder; keep the old kind/Part::key numbering.
        (super::semantics::key("scope drive", &self.stub, Part { pin: 0, ..part }.key())
            ^ LiveId::from_str(&self.pins[part.pin].pin).0).max(1)
    }

    fn each_part(&self, mut visit: impl FnMut(Part)) {
        for (pin, value) in self.pins.iter().enumerate() {
            for kind in [Kind::Pin, Kind::High, Kind::Low, Kind::Pwm, Kind::Input] { visit(Part { pin, kind }); }
            match value.drive {
                Drive::Pwm { .. } => { visit(Part { pin, kind: Kind::Frequency }); visit(Part { pin, kind: Kind::Duty }); }
                Drive::Level(_) => visit(Part { pin, kind: Kind::Level }),
                _ => {}
            }
        }
    }

    pub(super) fn lookup(&self, cx: &Cx, lookup: &super::semantics::Lookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let shift = self.area.rect(cx).pos - self.drawn_at;
        let clip = makepad_platform::a11y::snapshot_rect(&self.area, cx);
        self.each_part(|part| {
            let key = self.lookup_key(part);
            if !lookup.wants(key) { return; }
            let drawn = self.parts.iter().rev().find(|p| p.control.part == part);
            let value = drawn.map(|p| p.control.label.clone()).unwrap_or_else(|| match (part.kind, &self.pins[part.pin].drive) {
                (Kind::Frequency, Drive::Pwm { freq, .. }) => eng(*freq, "Hz", 3),
                (Kind::Duty, Drive::Pwm { duty, .. }) => format!("{:.1} % high", duty * 100.0),
                (Kind::Pin, _) => self.pins[part.pin].pin.clone(),
                _ => self.part_name(part),
            });
            let mut node = a11y_tree::Node {
                role: if part.knob() { a11y_tree::Role::Slider } else if part.kind == Kind::Level { a11y_tree::Role::StaticText } else { a11y_tree::Role::Button },
                name: self.part_name(part), value,
                description: if matches!(self.pins[part.pin].edit, BenchEdit::Derived { .. }) { COMPUTED.into() } else { String::new() },
                actions: a11y_tree::Actions::FOCUS | if !self.interactive(part) { a11y_tree::Actions::default() } else if part.knob() {
                    a11y_tree::Actions::SET_VALUE | a11y_tree::Actions::INCREMENT | a11y_tree::Actions::DECREMENT
                } else { a11y_tree::Actions::PRESS },
                ..Default::default()
            };
            node.states.set(a11y_tree::States::SELECTED, drawn.is_some_and(|p| p.control.selected));
            node.states.set(a11y_tree::States::FOCUSABLE, true);
            node.states.set(a11y_tree::States::FOCUSED, self.focused == Some(part) && cx.has_key_focus(self.area));
            node.states.set(a11y_tree::States::READ_ONLY, !self.interactive(part));
            if let Drive::Pwm { freq, duty } = self.pins[part.pin].drive {
                node.range = match part.kind {
                    Kind::Frequency => Some(a11y_tree::RangeValue { min: 1.0, max: 10e6, now: freq, step: div_steps(freq, 1) - freq }),
                    Kind::Duty => Some(a11y_tree::RangeValue { min: 0.0, max: 100.0, now: duty * 100.0, step: 1.0 }),
                    _ => None,
                };
            }
            let rect = drawn.zip(clip).and_then(|(p, clip)| super::semantics::intersect(Rect { pos: p.rect.pos + shift, ..p.rect }, clip))
                .filter(|rect| {
                    let (x, y) = makepad_platform::a11y::semantic_rect(*rect).center();
                    self.part_at(dvec2(x as f64, y as f64) - shift) == Some(part)
                });
            super::semantics::bounds(&mut node, rect);
            lookup.push(key, node, out);
        });
    }

    pub(super) fn lookup_action(&mut self, cx: &mut Cx, key: u64, request: &a11y_tree::ActionRequest) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions as A};
        let mut part = None;
        self.each_part(|candidate| { if self.lookup_key(candidate) == key { part = Some(candidate); } });
        let Some(part) = part else { return ActionResult::Unhandled };
        if request.action == A::FOCUS || request.action == A::SCROLL_INTO_VIEW {
            self.focused = Some(part);
            cx.set_key_focus(self.area);
            self.area.redraw(cx);
        } else if !self.interactive(part) || !matches!(self.pins[part.pin].edit, BenchEdit::Literal) && part.kind != Kind::Pin {
            return ActionResult::Unsupported;
        } else if request.action == A::PRESS && !part.knob() {
            self.activate(cx, part);
        } else if request.action == A::SET_VALUE && part.knob() {
            let unit = if part.kind == Kind::Frequency { "Hz" } else { "%" };
            let Some(value) = super::semantics::number(&request.arg, unit) else { return ActionResult::Unsupported };
            if self.drag.is_some() || if part.kind == Kind::Frequency { !(1.0..=10e6).contains(&value) } else { !(0.0..=100.0).contains(&value) } { return ActionResult::Unsupported; }
            self.set_value(cx, part, value);
        } else if (request.action == A::INCREMENT || request.action == A::DECREMENT) && part.knob() {
            self.step(cx, part, if request.action == A::INCREMENT { 1 } else { -1 });
        } else { return ActionResult::Unsupported; }
        ActionResult::Handled
    }
}
