use super::{
    columns::Columns,
    cursor::{eng, snap, Cursors, Snap},
    draw::{draw_columns, DrawTraceSeg, TraceStyle},
    store::{range_minmax, value_at},
};
use fab_elec_sim::trace::{TraceStore, CHUNK};
use makepad_widgets_core::*;
use std::{fmt::Write as _, sync::Arc};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    set_type_default() do #(DrawCursorRing::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        pixel: fn() {
            let p = (self.pos - vec2(0.5, 0.5)) * self.rect_size
            let distance = abs(length(p) - 3.0)
            let coverage = clamp((0.5 - distance) * self.draw_pass.dpi_factor + 0.5, 0.0, 1.0)
            let alpha = coverage * self.color.a
            return vec4(self.color.rgb * alpha, alpha)
        }
    }

    mod.widgets.TraceViewBase = #(TraceView::register_widget(vm))
    mod.widgets.TraceView = set_type_default() do mod.widgets.TraceViewBase {
        width: Fill height: Fill
        background: theme.color_bg_app
        grid: mix(theme.color_bg_app, theme.color_text, 0.12)
        grid_major: mix(theme.color_bg_app, theme.color_text, 0.28)
        ink: theme.color_text
        muted: mix(theme.color_bg_app, theme.color_text, 0.55)
        ch1: #d9bf52
        ch2: #54bccb
        ch3: #c77ebd
        ch4: #78ba71
        draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 9}}
    }
}

/// Supplies reduced traces and their default value ranges.
pub trait TraceSource {
    /// The record's time span in seconds, or None while empty.
    fn span(&self) -> Option<(f64, f64)>;
    /// Number of traces.
    fn traces(&self) -> usize;
    /// Clears and fills the trace's columns over the half-open window.
    fn read(&self, k: usize, t0: f64, t1: f64, cols: u32, out: &mut Columns);
    /// Default value range for a trace.
    fn range(&self, k: usize) -> (f64, f64);
    /// Interpolates a trace at a time in seconds.
    fn value_at(&self, k: usize, t: f64) -> Option<f32>;
    /// The trace's read-out name.
    fn name(&self, k: usize) -> String {
        format!("ch{}", k + 1)
    }
    /// The unit used by the trace's values.
    fn unit(&self, _k: usize) -> &'static str {
        "V"
    }
}

/// A trace hovered elsewhere in the editor.
#[derive(Clone)]
pub struct HoverTrace {
    pub source: Arc<dyn TraceSource + Send + Sync>,
    pub k: usize,
}

/// How a ghost's times map onto the live run's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GhostAlign {
    /// The same simulated time.
    Time,
    /// The ghost drawn this many seconds later than it ran.
    Shift(f64),
}

/// The run before an edit, drawn under the live traces.
pub struct Ghost {
    pub source: Arc<dyn TraceSource>,
    /// Each live trace's matching ghost trace, mapped by the host, or None.
    pub traces: Vec<Option<usize>>,
    pub align: GhostAlign,
}

impl Ghost {
    fn trace_index(&self, k: usize) -> Option<usize> {
        self.traces.get(k).copied().flatten().filter(|&g| g < self.source.traces())
    }
}

/// Shifts the nearest ghost event onto the live event nearest `at`.
/// Searches the ghost at the live event's time. Ties choose earlier events.
/// Returns None for empty lists or NaN `at`; both lists must be increasing.
pub fn align_by_marks(live: &[f64], ghost: &[f64], at: f64) -> Option<f64> {
    fn nearest(marks: &[f64], at: f64) -> Option<f64> {
        let next = marks.partition_point(|&time| time < at);
        let left = next.checked_sub(1).map(|i| marks[i]);
        let right = marks.get(next).copied();
        match (left, right) {
            (Some(left), Some(right)) => Some(if at - left <= right - at { left } else { right }),
            (left, right) => left.or(right),
        }
    }

    if at.is_nan() {
        return None;
    }
    let live = nearest(live, at)?;
    Some(live - nearest(ghost, live)?)
}

/// The ghost's own window for a live window under an alignment.
pub fn ghost_window(window: (f64, f64), align: GhostAlign) -> (f64, f64) {
    match align {
        GhostAlign::Time => window,
        GhostAlign::Shift(shift) => (window.0 - shift, window.1 - shift),
    }
}

/// Plain value arrays sharing one strictly increasing time array.
pub struct ArraySource {
    pub t: Arc<[f64]>,
    pub ys: Vec<Arc<[f32]>>,
}

impl TraceSource for ArraySource {
    fn span(&self) -> Option<(f64, f64)> {
        let first = *self.t.first()?;
        let last = *self.t.last()?;
        // The fitted half-open window includes the final sample.
        let end = last.next_up();
        (first.is_finite() && end.is_finite() && end > first).then_some((first, end))
    }

    fn traces(&self) -> usize {
        self.ys.len()
    }

    fn read(&self, k: usize, t0: f64, t1: f64, cols: u32, out: &mut Columns) {
        match self.ys.get(k) {
            Some(y) => out.reduce_into(&self.t, y, t0, t1, cols),
            None => out.reduce_into(&[], &[], t0, t1, cols),
        }
    }

    fn value_at(&self, k: usize, t: f64) -> Option<f32> {
        let y = self.ys.get(k)?;
        let &first = self.t.first()?;
        if y.len() != self.t.len() || t.is_nan() || t < first {
            return None;
        }
        let next = self.t.partition_point(|&time| time < t);
        if next == self.t.len() {
            return y.last().copied();
        }
        let right = y[next];
        if next == 0 || t == self.t[next] {
            return Some(right);
        }
        let left = f64::from(y[next - 1]);
        let fraction = (t - self.t[next - 1]) / (self.t[next] - self.t[next - 1]);
        Some((left + (f64::from(right) - left) * fraction) as f32)
    }

    fn range(&self, k: usize) -> (f64, f64) {
        let Some(y) = self.ys.get(k) else { return (-1.0, 1.0) };
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for &value in y.iter().filter(|v| v.is_finite()) {
            lo = lo.min(f64::from(value));
            hi = hi.max(f64::from(value));
        }
        if !lo.is_finite() {
            return (-1.0, 1.0);
        }
        if lo == hi {
            let pad = (lo.abs() * 0.05).max(1.0);
            return (lo - pad, hi + pad);
        }
        (lo, hi)
    }
}

/// A solver's trace store (C10): its signals read through the pyramid, so any zoom of any record
/// length costs a few bins per pixel column.
pub struct StoreSource {
    pub store: Arc<TraceStore>,
}

impl TraceSource for StoreSource {
    fn span(&self) -> Option<(f64, f64)> {
        let time = &self.store.time;
        if time.is_empty() {
            return None;
        }
        let (first, last) = (time.at(0), time.at(time.len() - 1));
        let end = last.next_up();
        (first.is_finite() && end.is_finite() && end > first).then_some((first, end))
    }

    fn traces(&self) -> usize {
        self.store.signals.len()
    }

    fn read(&self, k: usize, t0: f64, t1: f64, cols: u32, out: &mut Columns) {
        out.reduce_trace_into(&self.store, k, t0, t1, cols);
    }

    fn value_at(&self, k: usize, t: f64) -> Option<f32> {
        value_at(&self.store, k, t)
    }

    fn range(&self, k: usize) -> (f64, f64) {
        let Some(signal) = self.store.signals.get(k) else { return (-1.0, 1.0) };
        let n = signal.chunks.len() * CHUNK + signal.open.len();
        match range_minmax(signal, 0, n) {
            Some(m) if m.min.is_finite() && m.max.is_finite() && m.max > m.min => (f64::from(m.min), f64::from(m.max)),
            Some(m) if m.min.is_finite() && m.min == m.max => {
                let pad = (f64::from(m.min).abs() * 0.05).max(1.0);
                (f64::from(m.min) - pad, f64::from(m.max) + pad)
            }
            _ => (-1.0, 1.0),
        }
    }
}

const MIN_WINDOW: f64 = 1e-15;

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawCursorRing {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
}

#[derive(Clone, Copy, PartialEq)]
enum CursorId {
    A,
    B,
}

#[derive(Clone, Copy)]
struct TraceDrag {
    start: DVec2,
    window: (f64, f64),
    cursor: Option<CursorId>,
    place: CursorId,
    moved: bool,
    pointer: DVec2,
    snap: Snap,
}

/// Overlaid traces with a time graticule and pointer-centred zoom.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct TraceView {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_shape: DrawColor,
    #[live]
    draw_seg: DrawTraceSeg,
    // DrawColor is a plain DrawQuad with an instance colour.
    #[live]
    draw_grid: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_ring: DrawCursorRing,
    #[live]
    background: Vec4f,
    #[live]
    grid: Vec4f,
    #[live]
    grid_major: Vec4f,
    #[live]
    ink: Vec4f,
    #[live]
    muted: Vec4f,
    #[live]
    ch1: Vec4f,
    #[live]
    ch2: Vec4f,
    #[live]
    ch3: Vec4f,
    #[live]
    ch4: Vec4f,
    #[rust]
    area: Area,
    #[rust]
    view: Rect,
    #[rust]
    source: Option<Arc<dyn TraceSource + Send + Sync>>,
    #[rust]
    ghost: Option<Ghost>,
    #[rust]
    hover: Option<HoverTrace>,
    #[rust((0.0, 1.0))]
    window: (f64, f64),
    #[rust]
    drag: Option<TraceDrag>,
    #[rust]
    columns: Vec<Columns>,
    #[rust]
    ghost_columns: Vec<Columns>,
    #[rust]
    hover_columns: Columns,
    // Clearing the range also invalidates columns when the hover source or trace changes.
    #[rust]
    hover_range: Option<(f64, f64)>,
    #[rust]
    cursors: Cursors,
    #[rust(1.0)]
    dpi: f64,
    // ArraySource ranges scan the record only when a source is attached.
    #[rust]
    ranges: Vec<(f64, f64)>,
    #[rust]
    labels: [String; 11],
    #[rust]
    labels_window: Option<(f64, f64)>,
}

impl TraceView {
    /// Attaches a source, caches its ranges and fits its complete span.
    pub fn set_source(&mut self, cx: &mut Cx, source: Arc<dyn TraceSource + Send + Sync>) {
        self.ranges.clear();
        self.ranges.extend((0..source.traces()).map(|k| source.range(k)));
        let span = source.span();
        self.source = Some(source);
        self.drag = None;
        self.columns.clear();
        if let Some((t0, t1)) = span {
            self.set_window(cx, t0, t1);
        }
        self.area.redraw(cx);
    }

    /// Shows a ghost under the live traces, or removes it, and redraws.
    pub fn set_ghost(&mut self, cx: &mut Cx, ghost: Option<Ghost>) {
        self.ghost = ghost;
        if self.ghost.is_none() {
            self.ghost_columns = Vec::new();
        }
        self.area.redraw(cx);
    }

    /// Returns the shown ghost.
    pub fn ghost(&self) -> Option<&Ghost> {
        self.ghost.as_ref()
    }

    /// Shows the hovered trace, or clears it, and redraws only when it changes.
    /// Emphasis costs nothing extra; a shadow reads one trace per window change and draws one more batch.
    pub fn set_hover(&mut self, cx: &mut Cx, hover: Option<HoverTrace>) {
        let unchanged = match (&self.hover, &hover) {
            (Some(old), Some(new)) => old.k == new.k && Arc::ptr_eq(&old.source, &new.source),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.hover = hover;
        self.hover_range = None;
        self.area.redraw(cx);
    }

    /// Returns the hovered trace.
    pub fn hover(&self) -> Option<&HoverTrace> {
        self.hover.as_ref()
    }

    /// Returns the shown half-open time window.
    pub fn window(&self) -> (f64, f64) {
        self.window
    }

    /// Returns the cursor times in seconds.
    pub fn cursors(&self) -> &Cursors {
        &self.cursors
    }

    /// Sets the cursor times and refreshes their read-out.
    pub fn set_cursors(&mut self, cx: &mut Cx, c: Cursors) {
        self.cursors = c;
        self.drag = None;
        self.area.redraw(cx);
    }

    /// Sets a finite window, limited to the source's zoom bounds.
    pub fn set_window(&mut self, cx: &mut Cx, t0: f64, t1: f64) {
        if !t0.is_finite() || !t1.is_finite() || t1 <= t0 || !(t1 - t0).is_finite() {
            return;
        }
        let duration = (t1 - t0).clamp(MIN_WINDOW, self.max_window());
        let end = (t0 + duration).max(t0.next_up());
        if end.is_finite() {
            self.window = (t0, end);
            self.area.redraw(cx);
        }
    }

    fn max_window(&self) -> f64 {
        self.source
            .as_ref()
            .and_then(|source| source.span())
            .filter(|(start, end)| start.is_finite() && end.is_finite() && end > start)
            .map(|(start, end)| ((end - start) * 1.2).clamp(MIN_WINDOW, f64::MAX))
            .unwrap_or(f64::MAX)
    }

    fn fit(&mut self, cx: &mut Cx) {
        if let Some((t0, t1)) = self.source.as_ref().and_then(|source| source.span()) {
            self.set_window(cx, t0, t1);
        }
    }

    fn plot(&self) -> Rect {
        Rect { pos: self.view.pos + dvec2(8.0, 8.0), size: dvec2((self.view.size.x - 16.0).max(0.0), (self.view.size.y - 26.0).max(0.0)) }
    }

    fn refresh_columns(&mut self, plot: Rect) {
        let Some(source) = &self.source else {
            self.columns.clear();
            return;
        };
        let count = (plot.size.x * self.dpi).ceil().max(1.0) as u32;
        self.columns.resize_with(self.ranges.len(), Columns::default);
        for (k, columns) in self.columns.iter_mut().enumerate() {
            source.read(k, self.window.0, self.window.1, count, columns);
        }
    }

    fn refresh_hover_columns(&mut self, plot: Rect) -> Option<(f64, f64)> {
        let hover = self.hover.as_ref().filter(|hover| hover.k < hover.source.traces())?;
        let count = (plot.size.x * self.dpi).ceil().max(1.0) as u32;
        let columns = &mut self.hover_columns;
        if self.hover_range.is_none() || columns.t0 != self.window.0 || columns.t1 != self.window.1 || columns.cols != count {
            hover.source.read(hover.k, self.window.0, self.window.1, count, columns);
        }
        Some(*self.hover_range.get_or_insert_with(|| {
            let (lo, hi) = hover.source.range(hover.k);
            let pad = (hi - lo) * 0.1;
            (lo - pad, hi + pad)
        }))
    }

    fn cursor_x(&self, time: Option<f64>, plot: Rect) -> Option<f64> {
        let time = time?;
        if !time.is_finite() || time < self.window.0 || time >= self.window.1 {
            return None;
        }
        Some(plot.pos.x + (time - self.window.0) / (self.window.1 - self.window.0) * plot.size.x)
    }

    fn cursor_at(&self, x: f64, plot: Rect, preferred: CursorId) -> Option<CursorId> {
        let a = self.cursor_x(self.cursors.a, plot).map(|p| (p - x).abs()).filter(|&d| d <= 4.0);
        let b = self.cursor_x(self.cursors.b, plot).map(|p| (p - x).abs()).filter(|&d| d <= 4.0);
        match (a, b) {
            (Some(a), Some(b)) if a == b => Some(preferred),
            (Some(a), Some(b)) => Some(if a < b { CursorId::A } else { CursorId::B }),
            (Some(_), None) => Some(CursorId::A),
            (None, Some(_)) => Some(CursorId::B),
            _ => None,
        }
    }

    fn place_cursor(&mut self, cx: &mut Cx, id: CursorId, pointer: DVec2, plot: Rect) -> Snap {
        if plot.size.x <= 0.0 {
            return Snap::Free;
        }
        let count = (plot.size.x * self.dpi).ceil().max(1.0) as u32;
        // An input event can arrive after zooming but before the next frame.
        if self.columns.len() != self.ranges.len() || self.columns.iter().any(|c| c.t0 != self.window.0 || c.t1 != self.window.1 || c.cols != count) {
            self.refresh_columns(plot);
        }
        let fraction = ((pointer.x - plot.pos.x) / plot.size.x).clamp(0.0, 1.0);
        let x = (fraction * f64::from(count)) as f32;
        let radius = (6.0 * f64::from(count) / (plot.size.x * self.dpi)) as f32;
        let mut position = (x, Snap::Free);
        for (columns, &(lo, hi)) in self.columns.iter().zip(&self.ranges) {
            let candidate = snap(columns, x, radius, (lo * 0.5 + hi * 0.5) as f32);
            if candidate.1 != Snap::Free {
                position = candidate;
                break;
            }
        }
        let fraction = if position.1 == Snap::Free { fraction } else { f64::from(position.0) / f64::from(count) };
        let time = self.window.0 + fraction * (self.window.1 - self.window.0);
        match id {
            CursorId::A => self.cursors.a = Some(time),
            CursorId::B => self.cursors.b = Some(time),
        }
        self.area.redraw(cx);
        position.1
    }

    fn readout_lines(&self) -> Vec<String> {
        let mut lines = self.cursors.time_lines();
        if lines.is_empty() {
            return lines;
        }
        if let Some(source) = &self.source {
            for k in 0..source.traces() {
                let unit = source.unit(k);
                let a = self.cursors.a.and_then(|t| source.value_at(k, t)).map(f64::from);
                let b = self.cursors.b.and_then(|t| source.value_at(k, t)).map(f64::from);
                let value = |v: Option<f64>| v.map(|v| eng(v, unit, 4)).unwrap_or_else(|| "—".into());
                let mut line = source.name(k);
                if self.cursors.a.is_some() {
                    let _ = write!(line, "  A {}", value(a));
                }
                if self.cursors.b.is_some() {
                    let _ = write!(line, "  B {}", value(b));
                }
                if self.cursors.a.is_some() && self.cursors.b.is_some() {
                    let _ = write!(line, "  Δ{} {}", unit, value(a.zip(b).map(|(a, b)| b - a)));
                }
                if let Some(ghost) = &self.ghost {
                    let trace = ghost.trace_index(k);
                    for (label, time) in [("A", self.cursors.a), ("B", self.cursors.b)] {
                        if let Some(time) = time {
                            let time = ghost_window((time, time), ghost.align).0;
                            let v = trace.and_then(|g| ghost.source.value_at(g, time)).map(f64::from);
                            let _ = write!(line, "  ghost {} {}", label, value(v));
                        }
                    }
                }
                lines.push(line);
            }
        }
        lines
    }

    fn draw_cursors(&mut self, cx: &mut Cx2d, plot: Rect) {
        let pixel = 1.0 / self.dpi;
        let colors = [self.ch1, self.ch2, self.ch3, self.ch4];
        cx.push_clip_rect(plot);
        for (id, time, label) in [(CursorId::A, self.cursors.a, "A"), (CursorId::B, self.cursors.b, "B")] {
            let Some(x) = self.cursor_x(time, plot) else { continue };
            let mut color = self.ink;
            if id == CursorId::B {
                color.w *= 0.7;
            }
            self.draw_grid.color = color;
            self.draw_grid.draw_abs(cx, Rect { pos: dvec2((x * self.dpi).floor() / self.dpi, plot.pos.y), size: dvec2(pixel, plot.size.y) });
            self.draw_text.color = color;
            let label_x = (x + 3.0).min(plot.pos.x + (plot.size.x - 12.0).max(0.0));
            self.draw_text.draw_abs(cx, dvec2(label_x, plot.pos.y + 2.0), label);
            if let (Some(source), Some(time)) = (&self.source, time) {
                self.draw_ring.begin_many_instances(cx);
                for (k, &(lo, hi)) in self.ranges.iter().enumerate() {
                    let Some(value) = source.value_at(k, time).filter(|v| v.is_finite()) else { continue };
                    let span = hi - lo;
                    if !lo.is_finite() || !span.is_finite() || span <= 0.0 {
                        continue;
                    }
                    let y = plot.pos.y + plot.size.y * (1.0 - (f64::from(value) - lo) / span);
                    if !y.is_finite() || y < plot.pos.y || y > plot.pos.y + plot.size.y {
                        continue;
                    }
                    self.draw_ring.color = colors[k % colors.len()];
                    self.draw_ring.draw_abs(cx, Rect { pos: dvec2(x - 4.5, y - 4.5), size: dvec2(9.0, 9.0) });
                }
                self.draw_ring.end_many_instances(cx);
            }
        }
        let lines = self.readout_lines();
        if !lines.is_empty() {
            let text = lines.join("\n");
            let laid = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), &text);
            let size = dvec2(f64::from(laid.size_in_lpxs.width * self.draw_text.font_scale), f64::from(laid.size_in_lpxs.height * self.draw_text.font_scale));
            let pos = plot.pos + dvec2(4.0, 16.0);
            self.draw_shape.color = self.background;
            self.draw_shape.color.w *= 0.85;
            self.draw_shape.draw_abs(cx, Rect { pos, size: size + dvec2(12.0, 8.0) });
            self.draw_text.color = self.ink;
            self.draw_text.draw_abs(cx, pos + dvec2(6.0, 4.0), &text);
        }
        if let Some(drag) = self.drag.filter(|d| d.cursor.is_some()) {
            let text = match drag.snap {
                Snap::Sample => "sample",
                Snap::Crossing => "edge",
                Snap::Peak => "peak",
                Snap::Trough => "trough",
                Snap::Free => {
                    if drag.cursor == Some(CursorId::A) {
                        "A"
                    } else {
                        "B"
                    }
                }
            };
            let time = if drag.cursor == Some(CursorId::A) { self.cursors.a } else { self.cursors.b };
            if let Some(x) = self.cursor_x(time, plot) {
                let laid = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
                let size =
                    dvec2(f64::from(laid.size_in_lpxs.width * self.draw_text.font_scale), f64::from(laid.size_in_lpxs.height * self.draw_text.font_scale));
                let pos = dvec2(
                    (x + 6.0).min(plot.pos.x + (plot.size.x - size.x - 4.0).max(0.0)),
                    (drag.pointer.y + 8.0).clamp(plot.pos.y, plot.pos.y + (plot.size.y - size.y - 4.0).max(0.0)),
                );
                self.draw_shape.color = self.background;
                self.draw_shape.color.w *= 0.85;
                self.draw_shape.draw_abs(cx, Rect { pos, size: size + dvec2(4.0, 4.0) });
                self.draw_text.color = self.ink;
                self.draw_text.draw_abs(cx, pos + dvec2(2.0, 2.0), text);
            }
        }
        cx.pop_clip_rect();
    }

    fn draw_graticule(&mut self, cx: &mut Cx2d, plot: Rect) {
        let dpi = cx.current_dpi_factor();
        let pixel = 1.0 / dpi;
        let snap = |v: f64| (v * dpi).floor() / dpi;
        let mid_x = snap(plot.pos.x + plot.size.x * 0.5);
        let mid_y = snap(plot.pos.y + plot.size.y * 0.5);
        self.draw_grid.begin_many_instances(cx);
        for i in 0..=10 {
            let x = snap(plot.pos.x + plot.size.x * f64::from(i) / 10.0);
            self.draw_grid.color = if i == 5 { self.grid_major } else { self.grid };
            self.draw_grid.draw_abs(cx, Rect { pos: dvec2(x, plot.pos.y), size: dvec2(pixel, plot.size.y) });
        }
        for i in 0..=8 {
            let y = snap(plot.pos.y + plot.size.y * f64::from(i) / 8.0);
            self.draw_grid.color = if i == 4 { self.grid_major } else { self.grid };
            self.draw_grid.draw_abs(cx, Rect { pos: dvec2(plot.pos.x, y), size: dvec2(plot.size.x, pixel) });
        }
        self.draw_grid.color = self.grid_major;
        for i in 1..50 {
            if i % 5 != 0 {
                let x = snap(plot.pos.x + plot.size.x * f64::from(i) / 50.0);
                self.draw_grid.draw_abs(cx, Rect { pos: dvec2(x, mid_y - 2.0), size: dvec2(pixel, 4.0 + pixel) });
            }
        }
        for i in 1..40 {
            if i % 5 != 0 {
                let y = snap(plot.pos.y + plot.size.y * f64::from(i) / 40.0);
                self.draw_grid.draw_abs(cx, Rect { pos: dvec2(mid_x - 2.0, y), size: dvec2(4.0 + pixel, pixel) });
            }
        }
        self.draw_grid.end_many_instances(cx);
    }

    fn draw_axis(&mut self, cx: &mut Cx2d, plot: Rect) {
        if self.labels_window != Some(self.window) {
            let step = (self.window.1 - self.window.0) / 10.0;
            let top = self.window.0.abs().max(self.window.1.abs());
            for (i, label) in self.labels.iter_mut().enumerate() {
                let time = self.window.0 + step * i as f64;
                time_label(label, time, step, top);
            }
            self.labels_window = Some(self.window);
        }
        self.draw_text.color = self.muted;
        self.draw_text.begin_many_instances(cx);
        for (i, label) in self.labels.iter().enumerate() {
            let laid = self.draw_text.layout(cx, 0.0, 0.0, None, false, Align::default(), label);
            let width = f64::from(laid.size_in_lpxs.width * self.draw_text.font_scale);
            let x = plot.pos.x + plot.size.x * i as f64 / 10.0 - width * 0.5;
            let right = (self.view.pos.x + self.view.size.x - width).max(self.view.pos.x);
            self.draw_text.draw_abs(cx, dvec2(x.clamp(self.view.pos.x, right), plot.pos.y + plot.size.y + 3.0), label);
        }
        self.draw_text.end_many_instances(cx);
    }
}

/// A graticule time in engineering notation with the figures its axis needs: enough for one
/// division `step` to change the label at the axis' largest time `top`, so a deep zoom far from
/// zero reads `50.003 µs`, `50.016 µs` rather than one repeated value.
fn time_label(out: &mut String, time: f64, step: f64, top: f64) {
    out.clear();
    if time == 0.0 || (step > 0.0 && time.abs() < step * 1e-6) {
        out.push_str("0 s");
        return;
    }
    let sig = if step > 0.0 && top > 0.0 { (top.log10().floor() - step.log10().floor()) as i64 + 2 } else { 3 };
    out.push_str(&eng(time, "s", sig.clamp(3, 9) as usize));
}

impl WidgetNode for TraceView {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }
    fn walk(&mut self, _: &mut Cx) -> Walk {
        self.walk
    }
    fn area(&self) -> Area {
        self.area
    }
    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
    fn children(&self, _visit: &mut dyn FnMut(LiveId, WidgetRef)) {}
    fn find_widgets_from_point(&self, _cx: &Cx, _p: DVec2, _found: &mut dyn FnMut(&WidgetRef)) {}
}

impl Widget for TraceView {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, self.view);
        cx.push_clip_rect(self.view);
        self.draw_shape.color = self.background;
        self.draw_shape.draw_abs(cx, self.view);
        self.dpi = cx.current_dpi_factor();
        let plot = self.plot();
        if plot.size.x > 0.0 && plot.size.y > 0.0 {
            self.draw_graticule(cx, plot);
            self.refresh_columns(plot);
            let colors = [self.ch1, self.ch2, self.ch3, self.ch4];
            if let Some(ghost) = &self.ghost {
                let (t0, t1) = ghost_window(self.window, ghost.align);
                self.ghost_columns.resize_with(self.columns.len(), Columns::default);
                for (k, ((columns, live), &(lo, hi))) in self.ghost_columns.iter_mut().zip(&self.columns).zip(&self.ranges).enumerate() {
                    let Some(g) = ghost.trace_index(k) else { continue };
                    ghost.source.read(g, t0, t1, live.cols, columns);
                    let mut color = colors[k % colors.len()].mix(self.muted, 0.55);
                    color.w = 0.5;
                    let style = TraceStyle { color, width: 1.25, dot: 0.0 };
                    draw_columns(cx, &mut self.draw_seg, columns, plot, lo, hi, &style);
                }
            }
            let emphasis = self
                .hover
                .as_ref()
                .filter(|hover| hover.k < self.columns.len() && self.source.as_ref().is_some_and(|source| Arc::ptr_eq(source, &hover.source)))
                .map(|hover| hover.k);
            if emphasis.is_none() {
                if let Some((lo, hi)) = self.refresh_hover_columns(plot) {
                    let mut color = self.muted;
                    color.w *= 0.5;
                    let style = TraceStyle { color, width: 1.25, dot: 0.0 };
                    draw_columns(cx, &mut self.draw_seg, &self.hover_columns, plot, lo, hi, &style);
                }
            }
            for (k, (columns, &(lo, hi))) in self.columns.iter().zip(&self.ranges).enumerate() {
                if emphasis == Some(k) {
                    continue;
                }
                let mut color = colors[k % colors.len()];
                if emphasis.is_some() {
                    color.w *= 0.4;
                }
                let style = TraceStyle { color, width: 1.5, dot: 4.0 };
                draw_columns(cx, &mut self.draw_seg, columns, plot, lo, hi, &style);
            }
            if let Some(k) = emphasis {
                let (lo, hi) = self.ranges[k];
                let style = TraceStyle { color: colors[k % colors.len()], width: 2.25, dot: 4.0 };
                draw_columns(cx, &mut self.draw_seg, &self.columns[k], plot, lo, hi, &style);
            }
            self.draw_axis(cx, plot);
            self.draw_cursors(cx, plot);
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let placed = self.area.rect(cx);
        if placed.size.x > 0.0 {
            self.view = placed;
        }
        let plot = self.plot();
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => {
                // the plot drags: a hand while hovering it (the drag rule)
                cx.set_cursor(if self.drag.is_some() { MouseCursor::Grabbing } else { MouseCursor::Hand });
            }
            Hit::FingerScroll(e) if plot.contains(e.abs) && plot.size.x > 0.0 => {
                let old_width = self.window.1 - self.window.0;
                // Wheel deltas use 40 logical points per notch; trackpads stay continuous.
                let width = (old_width * 1.15_f64.powf(e.scroll.y / 40.0)).clamp(MIN_WINDOW, self.max_window());
                let fraction = ((e.abs.x - plot.pos.x) / plot.size.x).clamp(0.0, 1.0);
                let anchor = self.window.0 + fraction * old_width;
                let start = anchor - fraction * width;
                self.set_window(cx, start, start + width);
            }
            Hit::FingerDown(e) if e.device.is_primary_hit() && plot.contains(e.abs) => {
                cx.set_key_focus_from_user(self.area);
                let place = if e.modifiers.shift { CursorId::B } else { CursorId::A };
                self.drag = Some(TraceDrag {
                    start: e.abs,
                    window: self.window,
                    cursor: self.cursor_at(e.abs.x, plot, place),
                    place,
                    moved: false,
                    pointer: e.abs,
                    snap: Snap::Free,
                });
                cx.set_cursor(MouseCursor::Grabbing);
                self.area.redraw(cx);
            }
            Hit::FingerMove(e) => {
                if let Some(mut drag) = self.drag {
                    drag.pointer = e.abs;
                    drag.moved |= (e.abs - drag.start).length() > 3.0;
                    if drag.moved && plot.size.x > 0.0 {
                        if let Some(id) = drag.cursor {
                            drag.snap = self.place_cursor(cx, id, e.abs, plot);
                        } else {
                            let shift = (e.abs.x - drag.start.x) * (drag.window.1 - drag.window.0) / plot.size.x;
                            self.set_window(cx, drag.window.0 - shift, drag.window.1 - shift);
                        }
                    }
                    self.drag = Some(drag);
                    cx.set_cursor(MouseCursor::Grabbing);
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(e) => {
                if let Some(drag) = self.drag.take() {
                    let moved = drag.moved || (e.abs - drag.start).length() > 3.0;
                    if !e.cancelled && !e.is_sweep {
                        if moved {
                            if let Some(id) = drag.cursor {
                                self.place_cursor(cx, id, e.abs, plot);
                            } else if plot.size.x > 0.0 {
                                let shift = (e.abs.x - drag.start.x) * (drag.window.1 - drag.window.0) / plot.size.x;
                                self.set_window(cx, drag.window.0 - shift, drag.window.1 - shift);
                            }
                        } else if e.is_over && plot.contains(e.abs) {
                            if e.tap_count >= 2 {
                                self.fit(cx);
                            } else {
                                self.place_cursor(cx, drag.place, e.abs, plot);
                            }
                        }
                    }
                    cx.set_cursor(MouseCursor::Hand);
                    self.area.redraw(cx);
                }
            }
            Hit::KeyDown(e) if e.key_code == KeyCode::Escape => {
                self.set_cursors(cx, Cursors::default());
                cx.set_cursor(MouseCursor::Hand);
            }
            _ => {}
        }
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Figure;
        makepad_platform::a11y::set_string(&mut node.name, "trace view");
        node.value.clear();
        let _ = write!(&mut node.value, "{} .. {}", self.window.0, self.window.1);
        for line in self.readout_lines() {
            node.value.push('\n');
            node.value.push_str(&line);
        }
    }
}
