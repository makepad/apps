//! Retained histogram bars shared by the answer line and its popover.

use super::{distribution::Distribution, scope::LabelCache};
use makepad_widgets_core::*;

pub(super) fn same_bins(a: &Distribution, b: &Distribution) -> bool {
    a.total == b.total && a.k == b.k && a.start == b.start && a.counts == b.counts
}

/// Bins the occupied range is moved right by, so it sits in the middle of the 64 (a spread of
/// one bin, such as an exact simulator's constant answer, is drawn centred, not at the left edge).
pub(super) fn centring(distribution: &Distribution) -> usize {
    let occupied = distribution.counts.iter().rposition(|&count| count > 0).map_or(0, |i| i + 1);
    (64 - occupied) / 2
}

pub(super) struct Histogram {
    draw_list: DrawList2d,
    heights: [f64; 64],
    shift: usize,
    color: Option<[f32; 4]>,
}

impl Histogram {
    pub fn new(cx: &mut Cx, distribution: &Distribution) -> Self {
        let mut histogram = Self { draw_list: DrawList2d::new(cx), heights: [0.0; 64], shift: 0, color: None };
        histogram.set(cx, distribution);
        histogram
    }

    pub fn set(&mut self, cx: &mut Cx, distribution: &Distribution) {
        let max = distribution.counts.iter().copied().max().unwrap_or(0).max(1) as f64;
        for (height, &count) in self.heights.iter_mut().zip(&distribution.counts) { *height = count as f64 / max; }
        self.shift = centring(distribution);
        self.draw_list.redraw(cx);
    }

    pub fn draw(&mut self, cx: &mut Cx2d, draw: &mut DrawColor, rect: Rect, color: [f32; 4]) {
        if self.color != Some(color) { self.color = Some(color); self.draw_list.redraw(cx); }
        if !self.draw_list.begin(cx, Walk::abs_rect(rect)).is_redrawing() { return; }
        draw.color = vec4(color[0], color[1], color[2], color[3] * 0.5);
        let width = rect.size.x / 64.0;
        for (i, &height) in self.heights.iter().enumerate() {
            if height == 0.0 { continue; }
            let height = height * rect.size.y;
            draw.draw_abs(cx, Rect { pos: rect.pos + dvec2((i + self.shift) as f64 * width, rect.size.y - height), size: dvec2(width, height) });
        }
        self.draw_list.end(cx);
    }
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.AnswerDistribution = set_type_default() do #(AnswerDistribution::register_widget(vm)) {
        width: Fill height: 38
        draw_text +: {text_style: theme.font_code {font_size: 11}}
    }
}

/// A retained child is needed to paint the bars inside the popover's own overlay.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct AnswerDistribution {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[live(true)] visible: bool,
    #[live] draw_shape: DrawColor,
    #[live] draw_text: DrawText,
    #[rust] area: Area,
    #[rust] histogram: Option<Histogram>,
    #[rust] distribution: Option<Distribution>,
    #[rust] labels: (String, String),
    #[rust] tick: Option<f64>,
    /// The occupied bins as fractions of the chart (start, end), after centring.
    #[rust] occupied: (f64, f64),
    #[rust] color: [f32; 4],
    #[rust] text_color: [f32; 4],
    #[rust] cache: LabelCache,
}

impl AnswerDistribution {
    pub fn set(&mut self, cx: &mut Cx, distribution: &Distribution, labels: (String, String), current: Option<f64>, color: [f32; 4], text_color: [f32; 4]) {
        if self.distribution.as_ref().is_none_or(|old| !same_bins(old, distribution)) {
            if let Some(histogram) = &mut self.histogram { histogram.set(cx, distribution); }
            else { self.histogram = Some(Histogram::new(cx, distribution)); }
            self.distribution = Some(distribution.clone());
        }
        self.labels = labels;
        // Position the current answer at the same precision as the recorded event values.
        let shift = centring(distribution) as f64;
        self.tick = current.map(|value| value as f32).filter(|value| value.is_finite() && distribution.total > 0)
            .map(|value| ((f64::from(value) * 2.0_f64.powi(-distribution.k) - distribution.start + shift) / 64.0).clamp(0.0, 1.0));
        self.occupied = (shift / 64.0, (64.0 - shift) / 64.0);
        self.color = color;
        self.text_color = text_color;
        self.area.redraw(cx);
    }
}

impl WidgetNode for AnswerDistribution {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
    fn visible(&self) -> bool { self.visible }
    fn set_visible(&mut self, cx: &mut Cx, visible: bool) {
        if self.visible == visible { return; }
        self.visible = visible;
        self.area.redraw(cx);
    }
}

impl Widget for AnswerDistribution {
    fn handle_event(&mut self, _: &mut Cx, _: &Event, _: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, _: &mut Scope, walk: Walk) -> DrawStep {
        if !self.visible { return DrawStep::done(); }
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        self.cache.begin_frame(&self.draw_text, base_theme(cx));
        cx.push_clip_rect(rect);
        let chart = Rect { size: dvec2(rect.size.x, 20.0), ..rect };
        if let Some(histogram) = &mut self.histogram { histogram.draw(cx, &mut self.draw_shape, chart, self.color); }
        if let Some(tick) = self.tick {
            let color = self.color;
            self.draw_shape.color = vec4(color[0], color[1], color[2], color[3]);
            self.draw_shape.draw_abs(cx, Rect { pos: chart.pos + dvec2(tick * (chart.size.x - 1.0).max(0.0), 0.0), size: dvec2(1.0, chart.size.y) });
        }
        let color = self.text_color;
        self.draw_text.color = vec4(color[0], color[1], color[2], color[3]);
        // The range's ends sit under the occupied bins; a range of one value is one centred label.
        let single = self.labels.0 == self.labels.1;
        for (text, right) in [(&self.labels.0, false), (&self.labels.1, true)] {
            if single && right { break; }
            let layout = self.cache.layout(cx, &self.draw_text, text);
            let size = dvec2(f64::from(layout.size_in_lpxs.width * self.draw_text.font_scale), f64::from(layout.size_in_lpxs.height * self.draw_text.font_scale));
            let free = (rect.size.x - size.x).max(0.0);
            let x = if single { free * 0.5 } else if right { (self.occupied.1 * rect.size.x - size.x).clamp(0.0, free) }
                else { (self.occupied.0 * rect.size.x).min(free) };
            let pos = rect.pos + dvec2(x, 22.0);
            self.draw_text.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos, size }), &layout);
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }
}
