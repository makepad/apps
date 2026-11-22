//! Firmware observations on the bench's time axis. Drawing only reads the supplied store.

use super::{
    fw_events::{label, source_name, FwEvent, FwEvents, FwKind, FwSource},
    look::InstrumentLook,
    scope::{LabelCache, GAP, PAD},
};
use fab_elec_design::FwAt;
use makepad_widgets_core::*;
use std::sync::Arc;

const HEAD: f64 = 18.0;
const ROW: f64 = 22.0;

mod registration {
    use super::*;

    script_mod! {
        use mod.prelude.widgets_internal.*
        use mod.widgets.*

        mod.widgets.FwLaneBase = #(FwLane::register_widget(vm))
        mod.widgets.FwLane = set_type_default() do mod.widgets.FwLaneBase {
            width: Fill height: Fit
            draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 10}}
            draw_label +: {color: theme.color_text text_style: theme.font_regular {font_size: 9}}
        }
    }
}

pub fn script_mod(vm: &mut ScriptVm) { registration::script_mod(vm); }

/// The trigger card's time plot in drawing coordinates and simulated seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeMap {
    pub x0: f64,
    pub width: f64,
    pub t0: f64,
    pub t1: f64,
}

impl TimeMap {
    fn valid(self) -> bool {
        self.x0.is_finite() && self.width.is_finite() && self.width > 0.0
            && (self.x0 + self.width).is_finite() && self.t0.is_finite() && self.t1.is_finite()
            && self.t1 > self.t0 && (self.t1 - self.t0).is_finite()
    }

    fn x(self, t: f64) -> f64 { self.x0 + (t - self.t0) / (self.t1 - self.t0) * self.width }
    fn t(self, x: f64) -> f64 { self.t0 + (x - self.x0) / self.width * (self.t1 - self.t0) }
}

/// One tick actually submitted for a source's pixel column; x is its rounded centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawnMark {
    pub seq: u64,
    pub source: FwSource,
    pub t: f64,
    pub x: f64,
    pub count: usize,
}

struct MarkPart {
    label: String,
    rect: Rect,
    color: [f32; 4],
    end: f64,
}

fn marks_event(mark: DrawnMark, end: f64, event: Option<FwEvent>) -> bool {
    event.is_some_and(|event| event.source == mark.source && event.t >= mark.t && event.t < end)
}

#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct FwLane {
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
    #[rust]
    text_cache: LabelCache,
    #[rust]
    label_cache: LabelCache,
    #[rust]
    area: Area,
    #[rust]
    drawn_at: DVec2,
    #[rust]
    events: Option<Arc<dyn FwEvents>>,
    #[rust]
    cursor: Option<FwAt>,
    #[rust]
    time_map: Option<TimeMap>,
    #[rust]
    marks: Vec<DrawnMark>,
    #[rust]
    parts: Vec<MarkPart>,
    #[rust]
    shown_lost: u64,
    #[rust(true)]
    dark: bool,
}

/// An event's time for the lookup, to the nanosecond in milliseconds: neighbours a few microseconds
/// apart, which a four-digit axis label merges, stay apart.
fn precise_ms(t: f64) -> String {
    let text = format!("{:.6}", t * 1e3);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} ms")
}

fn event_color(kind: FwKind, look: &InstrumentLook) -> [f32; 4] {
    match kind {
        FwKind::Write { .. } => look.channel(0),
        FwKind::Duty { .. } => look.channel(1),
        FwKind::Read { .. } => look.channel(2),
        FwKind::Fault { .. } => look.canvas.violation,
        FwKind::Lost { .. } => look.canvas.warning,
        FwKind::Probe { .. } => look.channel(4),
        FwKind::Rx { .. } => look.channel(5),
        FwKind::Enter { .. } => look.channel(6),
        FwKind::Exit { .. } => look.channel(7),
        FwKind::Statement { .. } => look.channel(3),
    }
}

impl FwLane {
    pub fn set_events(&mut self, cx: &mut Cx, events: Option<Arc<dyn FwEvents>>) {
        self.events = events;
        self.marks.clear();
        self.parts.clear();
        self.shown_lost = 0;
        cx.widget_tree_mark_dirty(self.uid);
        self.area.redraw(cx);
    }

    pub fn set_cursor(&mut self, cx: &mut Cx, cursor: Option<FwAt>) {
        self.cursor = cursor;
        cx.widget_tree_mark_dirty(self.uid);
        self.area.redraw(cx);
    }

    fn cursor_event(&self) -> Option<FwEvent> {
        let cursor = self.cursor?;
        let seq = cursor.event?;
        let events = self.events.as_ref()?;
        let index = events.first_at(cursor.t.next_up()).checked_sub(1)?;
        // Resolve the carried identity, never substitute a different event from a newer store.
        events.get(index).filter(|event| event.seq == seq)
    }

    fn value(&self) -> String {
        let mut value = if self.shown_lost > 0 { format!("{} lost", self.shown_lost) } else { String::new() };
        if let Some(cursor) = self.cursor {
            let event = self.cursor_event().and_then(|event| self.events.as_ref().map(|events| label(events.as_ref(), event)))
                .unwrap_or_else(|| cursor.event.map_or_else(|| "between events".into(), |seq| format!("event {seq}")));
            let handler = cursor.handler.map(|id| self.events.as_ref().and_then(|events| events.handler_name(id)).map(str::to_owned).unwrap_or_else(|| id.to_string()));
            if !value.is_empty() { value.push_str("; "); }
            value.push_str(&format!("at cursor: {event}"));
            if let Some(handler) = handler { value.push_str(&format!(" in {handler}")); }
            else { value.push_str(", no handler"); }
        }
        value
    }

    /// Set immediately before drawing, in the same coordinate space as the scope card.
    pub fn set_time_map(&mut self, map: Option<TimeMap>) {
        self.time_map = map.filter(|map| map.valid());
    }

    pub fn drawn_marks(&self) -> &[DrawnMark] { &self.marks }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        self.draw_shape.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_shape.draw_abs(cx, rect);
    }

    fn text_width(&mut self, cx: &mut Cx2d, text: &str, numeric: bool) -> f64 {
        if numeric { self.text_cache.get(cx, &self.draw_text, text).width }
        else { self.label_cache.get(cx, &self.draw_label, text).width }
    }

    fn text_box(&mut self, cx: &mut Cx2d, rect: Rect, text: &str, color: [f32; 4], numeric: bool) {
        if rect.size.x <= 0.0 || text.is_empty() { return; }
        let (draw, cache) = if numeric { (&mut self.draw_text, &mut self.text_cache) } else { (&mut self.draw_label, &mut self.label_cache) };
        let layout = cache.get(cx, draw, text).fit(cx, draw, rect.size.x);
        draw.color = vec4(color[0], color[1], color[2], color[3]);
        cx.push_clip_rect(rect);
        draw.draw_walk_laidout(cx, Walk::abs_rect(rect), &layout);
        cx.pop_clip_rect();
    }

    fn handler_bar(&mut self, cx: &mut Cx2d, events: &dyn FwEvents, handler: u16, left: f64, right: f64, y: f64) {
        if right <= left { return; }
        let ink = InstrumentLook::for_theme(self.dark).channel(handler as usize);
        let alpha = if self.cursor.is_some_and(|cursor| cursor.handler == Some(handler)) { 1.0 } else { 0.25 };
        self.quad(cx, Rect { pos: dvec2(left, y + ROW - 3.0), size: dvec2(right - left, 3.0) }, [ink[0], ink[1], ink[2], alpha]);
        let label = events.handler_name(handler).map(str::to_owned).unwrap_or_else(|| handler.to_string());
        let width = self.text_width(cx, &label, false);
        if width + GAP <= right - left {
            self.text_box(cx, Rect { pos: dvec2(left + GAP, y + 12.0), size: dvec2(width, ROW - 12.0) }, &label, ink, false);
        }
    }

    fn draw_row(&mut self, cx: &mut Cx2d, events: &dyn FwEvents, source: FwSource, map: TimeMap, y: f64, plot: Rect) {
        let look = InstrumentLook::for_theme(self.dark);
        let dpi = cx.current_dpi_factor();
        let first_pixel = (plot.pos.x * dpi).floor();
        let columns = ((plot.pos.x + plot.size.x) * dpi).ceil() - first_pixel;
        let start = self.marks.len();
        let cursor_event = self.cursor_event();
        let mut active = None;
        let mut bar_start = plot.pos.x;
        cx.push_clip_rect(plot);
        // Each row has its own partition points. No loop depends on the event count.
        for column in 0..columns as usize {
            let left = ((first_pixel + column as f64) / dpi).max(plot.pos.x);
            let right = ((first_pixel + column as f64 + 1.0) / dpi).min(plot.pos.x + plot.size.x);
            let t0 = if left <= map.x0 { map.t0 } else { map.t(left) };
            let t1 = if right >= map.x0 + map.width { map.t1 } else { map.t(right) };
            if source == FwSource::Trace {
                let handler = events.handler_at(t0);
                if handler != active {
                    if let Some((id, _)) = active { self.handler_bar(cx, events, id, bar_start, left, y); }
                    active = handler;
                    bar_start = left;
                }
            }
            let first = events.first_at_source(source, t0);
            let end = events.first_at_source(source, t1);
            if end <= first { continue; }
            let Some(event) = events.get_source(source, first) else { continue };
            if event.t < t0 || event.t >= t1 { continue; }
            let x = (map.x(event.t) * dpi).round() / dpi;
            let count = end - first;
            let rect = Rect { pos: dvec2(x - 1.0, y + 1.0), size: dvec2(2.0, if count > 1 { 11.0 } else { 8.0 }) };
            let color = event_color(event.kind, look);
            let mark = DrawnMark { seq: event.seq, source, t: event.t, x, count };
            if marks_event(mark, t1, cursor_event) {
                self.quad(cx, Rect { pos: rect.pos - dvec2(1.0, 1.0), size: rect.size + dvec2(2.0, 2.0) }, look.text);
            }
            self.quad(cx, rect, color);
            self.marks.push(mark);
            self.parts.push(MarkPart { label: label(events, event), rect: super::semantics::intersect(rect, plot).unwrap_or_default(), color, end: t1 });
        }
        if let Some((id, _)) = active { self.handler_bar(cx, events, id, bar_start, plot.pos.x + plot.size.x, y); }
        let lost = if self.shown_lost > 0 { format!("{} lost", self.shown_lost) } else { String::new() };
        let lost_width = if lost.is_empty() { 0.0 } else { self.text_width(cx, &lost, true) + GAP };
        let mut next_label = plot.pos.x + plot.size.x;
        // Right to left: every accepted label fits before the next label already drawn.
        for index in (start..self.marks.len()).rev() {
            let x = self.marks[index].x + GAP;
            let label = std::mem::take(&mut self.parts[index].label);
            let width = self.text_width(cx, &label, true);
            if x >= plot.pos.x + lost_width && x + width + GAP <= next_label {
                self.text_box(cx, Rect { pos: dvec2(x, y), size: dvec2(width, 12.0) }, &label, self.parts[index].color, true);
                let left = self.parts[index].rect.pos.x;
                self.parts[index].rect = super::semantics::intersect(Rect { pos: dvec2(left, y), size: dvec2(x + width - left, 12.0) }, plot).unwrap_or_default();
                next_label = x;
            }
            self.parts[index].label = label;
        }
        if !lost.is_empty() {
            self.text_box(cx, Rect { pos: dvec2(plot.pos.x, y), size: dvec2(lost_width.min(plot.size.x), 12.0) }, &lost, look.canvas.warning, true);
        }
        cx.pop_clip_rect();
    }

    pub(super) fn lookup(&self, cx: &Cx, lookup: &super::semantics::Lookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let shift = self.area.rect(cx).pos - self.drawn_at;
        let clip = makepad_platform::a11y::snapshot_rect(&self.area, cx);
        let mut group = a11y_tree::Node { role: a11y_tree::Role::Group, name: "firmware".into(), value: self.value(), ..Default::default() };
        super::semantics::bounds(&mut group, clip);
        lookup.push(super::semantics::key("firmware", "card", 0), group, out);
        let cursor_event = self.cursor_event();
        for (mark, part) in self.marks.iter().zip(&self.parts) {
            let source = source_name(mark.source);
            let key = super::semantics::key("firmware event", source, mark.seq);
            if !lookup.wants(key) { continue; }
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Button,
                name: format!("{source} {} at {}", part.label, precise_ms(mark.t)),
                ..Default::default()
            };
            node.states.set(a11y_tree::States::SELECTED, marks_event(*mark, part.end, cursor_event));
            let rect = clip.and_then(|clip| super::semantics::intersect(Rect { pos: part.rect.pos + shift, ..part.rect }, clip));
            super::semantics::bounds(&mut node, rect);
            lookup.push(key, node, out);
        }
    }
}

impl WidgetNode for FwLane {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _point: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for FwLane {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, mut walk: Walk) -> DrawStep {
        let theme = base_theme(cx);
        self.dark = theme != BaseTheme::Light;
        let look = InstrumentLook::for_theme(self.dark);
        self.text_cache.begin_frame(&self.draw_text, theme);
        self.label_cache.begin_frame(&self.draw_label, theme);
        if walk.width.is_fit() { walk.width = Size::Fixed(600.0); }
        if walk.height.is_fit() { walk.height = Size::Fixed(PAD * 2.0 + HEAD + ROW * 2.0); }
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        self.drawn_at = view.pos;
        self.marks.clear();
        self.parts.clear();
        self.shown_lost = 0;
        let events = self.events.clone();
        if let (Some(events), Some(map)) = (&events, self.time_map) {
            if events.get(0).is_some_and(|oldest| map.t0 < oldest.t) { self.shown_lost = events.lost(); }
        }
        cx.begin_turtle(Walk::abs_rect(view), Layout::flow_overlay());
        cx.push_clip_rect(view);
        self.quad(cx, view, look.canvas.canvas);
        self.text_box(cx, Rect { pos: view.pos + dvec2(PAD, PAD), size: dvec2((view.size.x - PAD * 2.0).max(0.0), HEAD) }, "firmware", look.text, false);
        let left = self.time_map.map_or(view.pos.x + 64.0, |map| map.x0);
        for (row, source) in [FwSource::Image, FwSource::Trace].into_iter().enumerate() {
            let y = view.pos.y + PAD + HEAD + row as f64 * ROW;
            self.text_box(cx, Rect { pos: dvec2(view.pos.x + PAD, y), size: dvec2((left - view.pos.x - PAD - GAP).max(0.0), ROW) }, source_name(source), look.text_secondary, false);
            if let (Some(events), Some(map)) = (&events, self.time_map) {
                let plot = Rect { pos: dvec2(map.x0, y), size: dvec2(map.width, ROW) };
                if let Some(plot) = super::semantics::intersect(plot, view) { self.draw_row(cx, events.as_ref(), source, map, y, plot); }
            }
        }
        self.quad(cx, Rect { pos: view.pos + dvec2(PAD, view.size.y - 1.0), size: dvec2((view.size.x - PAD * 2.0).max(0.0), 1.0) }, look.zero_line);
        cx.pop_clip_rect();
        cx.end_turtle();
        DrawStep::done()
    }

    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "firmware");
        makepad_platform::a11y::set_string(&mut node.value, &self.value());
    }

    fn semantics_lookup(&self, cx: &Cx, lookup: &SemanticLookup, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let id_key = match lookup {
            SemanticLookup::Terms(q) => q.id_term().filter(|id| cx.a11y.action_owner(*id).uid() == self.uid.0).and_then(|id| cx.a11y.action_part_key(id)),
            SemanticLookup::Key(_) => None,
        };
        if let Some(lookup) = super::semantics::Lookup::new(lookup, id_key) { self.lookup(cx, &lookup, out); }
    }
}
