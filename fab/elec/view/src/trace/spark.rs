//! Small net traces with a shared hover shadow. An unchanged frame reads nothing; a moving
//! window reads 48 columns per visible spark, regardless of width. The host reads the hovered
//! trace once and shares its shadow with every visible spark.

use super::{
    columns::Columns,
    draw::{draw_columns, DrawTraceSeg, TraceStyle},
    view::TraceSource,
};
use crate::live::{ramp, volt_x, LiveLook};
use makepad_widgets_core::*;
use std::{fmt::Write as _, sync::Arc};

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    mod.widgets.NetSparkBase = #(NetSpark::register_widget(vm))
    mod.widgets.NetSpark = set_type_default() do mod.widgets.NetSparkBase {
        width: 96 height: 20
        hover_background: mix(theme.color_bg_app, theme.color_text, 0.08)
        muted: mix(theme.color_bg_app, theme.color_text, 0.55)
    }
}

/// Columns read by every spark, at any width.
pub const SPARK_COLS: u32 = 48;

/// One hovered trace, reduced once for all visible sparks.
pub struct SparkShadow {
    pub k: usize,
    pub columns: Columns,
    pub range: (f64, f64),
}

impl SparkShadow {
    /// Reads the hovered trace over the half-open window.
    pub fn read(source: &dyn TraceSource, k: usize, window: (f64, f64)) -> SparkShadow {
        let mut columns = Columns::default();
        source.read(k, window.0, window.1, SPARK_COLS, &mut columns);
        SparkShadow { k, columns, range: source.range(k) }
    }
}

/// The net whose spark the pointer entered or left.
#[derive(Clone, Debug, Default)]
pub enum NetSparkAction {
    #[default]
    None,
    Hovered(usize),
    Unhovered(usize),
}

#[derive(Clone, Copy, PartialEq)]
struct SparkKey {
    source: usize,
    k: usize,
    window: (f64, f64),
}

/// A net's voltage over time, coloured like its wire.
#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct NetSpark {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_seg: DrawTraceSeg,
    #[live]
    hover_background: Vec4f,
    #[live]
    muted: Vec4f,
    #[rust]
    area: Area,
    #[rust]
    source: Option<Arc<dyn TraceSource + Send + Sync>>,
    #[rust]
    k: usize,
    #[rust((0.0, 1.0))]
    window: (f64, f64),
    #[rust]
    columns: Columns,
    #[rust]
    columns_key: Option<SparkKey>,
    // Array ranges scan the record only when the source or trace changes.
    #[rust((-1.0, 1.0))]
    range: (f64, f64),
    #[rust]
    shadow: Option<Arc<SparkShadow>>,
    #[rust(LiveLook::dark())]
    look: LiveLook,
    #[rust]
    hovered: bool,
}

impl NetSpark {
    /// Attaches a trace and caches its range.
    pub fn set_trace(&mut self, cx: &mut Cx, source: Arc<dyn TraceSource + Send + Sync>, k: usize) {
        if self.k == k && self.source.as_ref().is_some_and(|old| Arc::ptr_eq(old, &source)) {
            return;
        }
        self.range = source.range(k);
        self.source = Some(source);
        self.k = k;
        self.columns_key = None;
        self.area.redraw(cx);
    }

    /// A scope capture already reduced on its measurement worker. Keep the same spark renderer
    /// without retaining or reducing raw acquisition samples on the UI thread.
    pub fn set_columns(&mut self, cx: &mut Cx, k: usize, columns: &Columns) {
        if self.source.is_none() && self.k == k && self.columns == *columns { return; }
        self.source = None;
        self.columns_key = None;
        self.k = k;
        self.columns.t0 = columns.t0;
        self.columns.t1 = columns.t1;
        self.columns.cols = columns.cols;
        self.columns.before = columns.before;
        self.columns.after = columns.after;
        self.columns.cols_data.clone_from(&columns.cols_data);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for column in &columns.cols_data {
            if column.lo.is_finite() { lo = lo.min(f64::from(column.lo)); }
            if column.hi.is_finite() { hi = hi.max(f64::from(column.hi)); }
        }
        self.range = if lo > hi { (-1.0, 1.0) } else if lo == hi {
            let pad = (lo.abs() * 0.05).max(1.0);
            (lo - pad, hi + pad)
        } else { (lo, hi) };
        self.area.redraw(cx);
    }

    /// Sets a finite, non-empty time window.
    pub fn set_window(&mut self, cx: &mut Cx, t0: f64, t1: f64) {
        if !t0.is_finite() || !t1.is_finite() || t1 <= t0 || !(t1 - t0).is_finite() || self.window == (t0, t1) {
            return;
        }
        self.window = (t0, t1);
        self.area.redraw(cx);
    }

    /// Shares the hovered trace, or clears it.
    pub fn set_shadow(&mut self, cx: &mut Cx, shadow: Option<Arc<SparkShadow>>) {
        let unchanged = match (&self.shadow, &shadow) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.shadow = shadow;
        self.area.redraw(cx);
    }

    /// Uses the same voltage colours as the live wires.
    pub fn set_look(&mut self, cx: &mut Cx, look: LiveLook) {
        if self.look == look {
            return;
        }
        self.look = look;
        self.area.redraw(cx);
    }

    /// Returns the attached trace index.
    pub fn trace(&self) -> Option<usize> {
        self.source.as_ref().map(|_| self.k)
    }

    fn refresh_columns(&mut self) {
        let Some(source) = &self.source else { return };
        let key = SparkKey { source: Arc::as_ptr(source) as *const () as usize, k: self.k, window: self.window };
        if self.columns_key == Some(key) {
            return;
        }
        source.read(self.k, self.window.0, self.window.1, SPARK_COLS, &mut self.columns);
        self.columns_key = Some(key);
    }
}

impl WidgetNode for NetSpark {
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

impl Widget for NetSpark {
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        cx.push_clip_rect(view);
        let own_shadow = self.shadow.as_ref().is_some_and(|shadow| self.trace() == Some(shadow.k));
        // No background of its own: the spark sits on its host's. The hover chip is a capsule of the
        // trace shader, drawn first, so it stays behind the traces (a quad of another shader can be
        // batched into an outer call that lies in front of them).
        if (self.hovered || own_shadow) && view.size.x > view.size.y && view.size.y > 0.0 {
            let r = view.size.y * 0.5;
            self.draw_seg.color = self.hover_background;
            self.draw_seg.width = view.size.y as f32;
            self.draw_seg.seg = vec4(r as f32, r as f32, (view.size.x - r) as f32, r as f32);
            self.draw_seg.draw_abs(cx, view);
        }
        let plot = Rect { pos: view.pos + dvec2(2.0, 2.0), size: dvec2((view.size.x - 4.0).max(0.0), (view.size.y - 4.0).max(0.0)) };
        if plot.size.x > 0.0 && plot.size.y > 0.0 {
            self.refresh_columns();
            if let Some(shadow) = self.shadow.as_ref().filter(|_| !own_shadow) {
                let mut color = self.muted;
                color.w *= 0.5;
                let style = TraceStyle { color, width: 1.0, dot: 0.0 };
                draw_columns(cx, &mut self.draw_seg, &shadow.columns, plot, shadow.range.0, shadow.range.1, &style);
            }
            let last = self.columns.cols_data.last().map(|column| (column.x1, column.y1)).filter(|(x, y)| x.is_finite() && y.is_finite());
            let color = last.map_or(self.muted, |(_, v)| ramp(self.look.volt_ramp, volt_x(v, &self.look)));
            let pad = (self.range.1 - self.range.0) * 0.1;
            let (lo, hi) = (self.range.0 - pad, self.range.1 + pad);
            let style = TraceStyle { color, width: 1.25, dot: 0.0 };
            draw_columns(cx, &mut self.draw_seg, &self.columns, plot, lo, hi, &style);
            if let Some((x, y)) = last {
                let span = hi - lo;
                if lo.is_finite() && span.is_finite() && span > 0.0 {
                    let x = plot.pos.x + f64::from(x) * plot.size.x / f64::from(SPARK_COLS);
                    let y = plot.pos.y + plot.size.y * (1.0 - (f64::from(y) - lo) / span);
                    if x.is_finite() && y.is_finite() {
                        self.draw_seg.color = color;
                        self.draw_seg.width = 3.0;
                        self.draw_seg.seg = vec4(2.5, 2.5, 2.5, 2.5);
                        self.draw_seg.draw_abs(cx, Rect { pos: dvec2(x - 2.5, y - 2.5), size: dvec2(5.0, 5.0) });
                    }
                }
            }
        }
        cx.pop_clip_rect();
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(_) => {
                self.hovered = true;
                cx.set_cursor(MouseCursor::Default);
                if let Some(k) = self.trace() {
                    cx.widget_action(self.uid, NetSparkAction::Hovered(k));
                }
                self.area.redraw(cx);
            }
            Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Default),
            Hit::FingerHoverOut(_) => {
                self.hovered = false;
                cx.set_cursor(MouseCursor::Default);
                if let Some(k) = self.trace() {
                    cx.widget_action(self.uid, NetSparkAction::Unhovered(k));
                }
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Figure;
        node.name.clear();
        node.value.clear();
        match &self.source {
            Some(source) => {
                node.name.push_str(&source.name(self.k));
                let last = self.columns.cols_data.last().map(|column| column.y1);
                let _ = match last {
                    Some(v) => write!(&mut node.value, "{} {}", v, source.unit(self.k)),
                    None => write!(&mut node.value, "no samples"),
                };
            }
            None => node.name.push_str("net spark"),
        }
    }
}
