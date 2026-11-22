use super::columns::Columns;
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.draw.DrawTraceSeg = set_type_default() do #(DrawTraceSeg::script_shader(vm)) {
        ..mod.draw.DrawQuad
        width: 1.5
        color: #fff

        pixel: fn() {
            let p = self.pos * self.rect_size
            let a = self.seg.xy
            let b = self.seg.zw
            let delta = b - a
            let length_sq = dot(delta, delta)
            var along = 0.0
            if length_sq > 0.0 {
                along = clamp(dot(p - a, delta) / length_sq, 0.0, 1.0)
            }
            let distance = length(p - a - delta * along)
            let dpi = self.draw_pass.dpi_factor
            let radius = max(self.width * 0.5, 0.5 / dpi)
            let coverage = clamp((radius - distance) * dpi + 0.5, 0.0, 1.0)
            let alpha = coverage * self.color.a
            return vec4(self.color.rgb * alpha, alpha)
        }
    }
}

/// An antialiased capsule. Its end points are relative to the quad's top-left corner, so a
/// layout pass that moves the quad after it was drawn moves the capsule with it.
#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawTraceSeg {
    #[deref]
    pub draw_super: DrawQuad,
    /// End points from the quad's top-left, in logical points, as (ax, ay, bx, by).
    #[live]
    pub seg: Vec4f,
    /// Stroke width in logical points.
    #[live]
    pub width: f32,
    /// Straight-alpha colour.
    #[live]
    pub color: Vec4f,
}

/// Trace colour, stroke width and sample-dot diameter in logical points.
pub struct TraceStyle {
    pub color: Vec4f,
    pub width: f32,
    pub dot: f32,
}

/// Draws column extrema, connecting samples and sparse dots in one batch.
pub fn draw_columns(
    cx: &mut Cx2d,
    seg: &mut DrawTraceSeg,
    cols: &Columns,
    plot: Rect,
    v_lo: f64,
    v_hi: f64,
    style: &TraceStyle,
) -> usize {
    let value_span = v_hi - v_lo;
    if cols.cols == 0 || plot.size.x <= 0.0 || plot.size.y <= 0.0
        || !v_lo.is_finite() || !value_span.is_finite() || value_span <= 0.0
    {
        return 0;
    }
    let x_of = |x: f32| plot.pos.x + f64::from(x) * plot.size.x / f64::from(cols.cols);
    let y_of = |y: f32| plot.pos.y + plot.size.y * (1.0 - (f64::from(y) - v_lo) / value_span);
    let point = |(x, y)| dvec2(x_of(x), y_of(y));
    let samples: u64 = cols.cols_data.iter().map(|c| u64::from(c.count)).sum();
    let spacing = match (cols.cols_data.first(), cols.cols_data.last()) {
        (Some(first), Some(last)) if samples > 1 =>
            (x_of(last.x1) - x_of(first.x0)) / (samples - 1) as f64,
        _ => plot.size.x,
    };
    let dots = cols.density() < 1.0 && spacing > 8.0 && style.dot > 0.0;
    let mut instances = 0;
    let mut previous = cols.before;
    seg.color = style.color;
    cx.push_clip_rect(plot);
    seg.begin_many_instances(cx);
    for column in &cols.cols_data {
        if let Some(prev) = previous {
            instances += capsule(cx, seg, point(prev), point((column.x0, column.y0)), style.width, plot);
        }
        let lo = y_of(column.lo);
        let hi = y_of(column.hi);
        if (hi - lo).abs() >= 0.5 {
            let x = (x_of(column.x0) + x_of(column.x1)) * 0.5;
            instances += capsule(cx, seg, dvec2(x, lo), dvec2(x, hi), style.width, plot);
        }
        if dots {
            let first = point((column.x0, column.y0));
            instances += capsule(cx, seg, first, first, style.dot, plot);
            if column.count > 1 {
                let last = point((column.x1, column.y1));
                instances += capsule(cx, seg, last, last, style.dot, plot);
            }
        }
        previous = Some((column.x1, column.y1));
    }
    if let (Some(prev), Some(next)) = (previous, cols.after) {
        instances += capsule(cx, seg, point(prev), point(next), style.width, plot);
    }
    seg.end_many_instances(cx);
    cx.pop_clip_rect();
    instances
}

fn capsule(cx: &mut Cx2d, seg: &mut DrawTraceSeg, a: DVec2, b: DVec2, width: f32, plot: Rect) -> usize {
    if !a.x.is_finite() || !a.y.is_finite() || !b.x.is_finite() || !b.y.is_finite() || !width.is_finite() {
        return 0;
    }
    let width = width.max((1.0 / cx.current_dpi_factor()) as f32);
    let pad = f64::from(width) * 0.5 + 1.0;
    // Clip before casting to f32: neighbours can be far away when deeply zoomed.
    let delta = b - a;
    let mut enter: f64 = 0.0;
    let mut leave: f64 = 1.0;
    for (origin, step, low, high) in [
        (a.x, delta.x, plot.pos.x - pad, plot.pos.x + plot.size.x + pad),
        (a.y, delta.y, plot.pos.y - pad, plot.pos.y + plot.size.y + pad),
    ] {
        if step == 0.0 {
            if origin < low || origin > high {
                return 0;
            }
        } else {
            let near = (low - origin) / step;
            let far = (high - origin) / step;
            enter = enter.max(near.min(far));
            leave = leave.min(near.max(far));
        }
    }
    if enter > leave {
        return 0;
    }
    let end = a + delta * leave;
    let start = a + delta * enter;
    let rect = Rect {
        pos: dvec2(start.x.min(end.x) - pad, start.y.min(end.y) - pad),
        size: dvec2((end.x - start.x).abs() + pad * 2.0, (end.y - start.y).abs() + pad * 2.0),
    };
    let (s, e) = (start - rect.pos, end - rect.pos);
    seg.seg = vec4(s.x as f32, s.y as f32, e.x as f32, e.y as f32);
    seg.width = width;
    seg.draw_abs(cx, rect);
    1
}
