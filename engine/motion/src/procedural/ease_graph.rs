//! `EaseGraph{curve: "cubic-bezier(0.7, 0, 0.3, 1)"}`: the curve-editor
//! graphic of an ease. A value grid and axes appear, the cubic-bezier draws
//! on from left to right with its two handles as diamonds on thin lines, an
//! accent playhead sweeps across with a value readout riding it, and below,
//! a spacing row of ghost circles (the ease's spacing: dense where it is
//! slow) with a ball travelling it. `fill_at` then grows the ball over the
//! frame (it becomes the next scene's background).
//!
//! The curve is solved with makepad-tween's CSS solver, so the graphic, the
//! readout and the ball agree with `Easing::css`.

use super::*;
use makepad_tween::Easing;

/// A cubic-bezier ease: its control points and the solver.
#[derive(Clone, Copy, Debug)]
pub struct EaseCurve {
    pub points: [f64; 4],
    easing: Easing,
}

impl EaseCurve {
    pub fn new(points: [f64; 4]) -> Self {
        let easing = Easing::css(points[0], points[1], points[2], points[3]);
        let points = [points[0].clamp(0.0, 1.0), points[1], points[2].clamp(0.0, 1.0), points[3]];
        Self { points, easing }
    }

    /// The eased value at progress `x` (0..1).
    pub fn value(&self, x: f64) -> f64 {
        if x <= 0.0 {
            0.0
        } else if x >= 1.0 {
            1.0
        } else {
            self.easing.map(x)
        }
    }

    /// The curve's point at bezier parameter `s`, in unit graph space.
    pub fn point(&self, s: f64) -> [f64; 2] {
        let [x1, y1, x2, y2] = self.points;
        let b = |p1: f64, p2: f64| 3.0 * (1.0 - s) * (1.0 - s) * s * p1 + 3.0 * (1.0 - s) * s * s * p2 + s * s * s;
        [b(x1, x2), b(y1, y2)]
    }

    /// `cubic-bezier(0.70, 0.00, 0.30, 1.00)`.
    pub fn css(&self) -> String {
        let [a, b, c, d] = self.points;
        format!("cubic-bezier({a:.2}, {b:.2}, {c:.2}, {d:.2})")
    }
}

/// An ease as written: `[x1, y1, x2, y2]`, `"cubic-bezier(x1, y1, x2, y2)"`,
/// a CSS name (`"ease"`, `"ease-in-out"`, …) or a Motion ease id
/// (`@ease_in_out_cubic`, as its CSS curve).
pub(crate) fn parse_curve(vm: &ScriptVm, value: Option<ScriptValue>) -> Result<EaseCurve, String> {
    const CSS: &[(&str, [f64; 4])] = &[
        ("ease", [0.25, 0.1, 0.25, 1.0]),
        ("ease-in", [0.42, 0.0, 1.0, 1.0]),
        ("ease-out", [0.0, 0.0, 0.58, 1.0]),
        ("ease-in-out", [0.42, 0.0, 0.58, 1.0]),
    ];
    let named = |name: &str| -> Option<[f64; 4]> {
        CSS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p).or_else(|| makepad_tween::CSS_PRESETS.iter().find(|(n, _)| *n == name).map(|(_, p)| *p))
    };
    let points = match value.filter(|v| !v.is_nil()) {
        None => [0.7, 0.0, 0.3, 1.0],
        Some(v) => {
            let nums = if obj::items(vm, v).is_some() { obj::items(vm, v).filter(|items| items.len() == 4).and_then(|items| items.into_iter().map(|x| obj::num(vm, x)).collect::<Option<Vec<f64>>>()) } else { None };
            if let Some(n) = nums {
                [n[0], n[1], n[2], n[3]]
            } else if let Some(name) = obj::ident(v) {
                named(&name).ok_or_else(|| format!("`@{name}` has no cubic-bezier form; write `curve: [x1, y1, x2, y2]`"))?
            } else if let Some(s) = obj::string(vm, v) {
                let s = s.trim();
                if let Some(inner) = s.strip_prefix("cubic-bezier(").and_then(|r| r.strip_suffix(')')) {
                    let parts: Vec<f64> = inner.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                    if parts.len() != 4 {
                        return Err(format!("`{s}` needs four numbers: cubic-bezier(x1, y1, x2, y2)"));
                    }
                    [parts[0], parts[1], parts[2], parts[3]]
                } else {
                    named(s).ok_or_else(|| format!("`{s}` is not a cubic-bezier(…) or a CSS ease name"))?
                }
            } else {
                return Err(format!("`curve` is \"cubic-bezier(x1, y1, x2, y2)\", [x1, y1, x2, y2] or an ease name, not {}", obj::kind(vm, v)));
            }
        }
    };
    if !(0.0..=1.0).contains(&points[0]) || !(0.0..=1.0).contains(&points[2]) {
        return Err(format!("a cubic-bezier's x1 and x2 are in 0..1 (got {} and {})", points[0], points[2]));
    }
    Ok(EaseCurve::new(points))
}

/// The share of the curve's length (in box space, `scale` per unit axis)
/// drawn when the playhead is at `x`: the curve drawn so far reaches the
/// playhead exactly.
fn trim_table(curve: &EaseCurve, scale: [f64; 2], samples: usize) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(samples + 1);
    let mut length = 0.0;
    let mut last = curve.point(0.0);
    out.push((0.0, 0.0));
    for i in 1..=samples {
        let p = curve.point(i as f64 / samples as f64);
        length += ((p[0] - last[0]) * scale[0]).hypot((p[1] - last[1]) * scale[1]);
        out.push((p[0], length));
        last = p;
    }
    let total = length.max(1e-9);
    out.iter().map(|(x, l)| (*x, l / total)).collect()
}

fn trim_at(table: &[(f64, f64)], x: f64) -> f64 {
    match table.iter().position(|(tx, _)| *tx >= x) {
        None => 1.0,
        Some(0) => 0.0,
        Some(i) => {
            let (x0, l0) = table[i - 1];
            let (x1, l1) = table[i];
            if x1 - x0 < 1e-12 { l1 } else { l0 + (l1 - l0) * (x - x0) / (x1 - x0) }
        }
    }
}

pub(super) fn lower(cx: &mut Cx, o: ScriptObject) -> Made {
    let curve = parse_curve(cx.vm, obj::opt(cx.vm, o, "curve")).map_err(|m| ("curve".to_string(), m))?;
    let [w, h] = get_vec2(cx, o, "size", [cx.doc[0] * 0.72, cx.doc[1] * 0.78])?;
    let color = get_color(cx, o, "color", 0xf0ebe0ff)?;
    let accent = get_color(cx, o, "accent", 0xff5a1fff)?;
    let ink = get_color(cx, o, "ink", 0xf0ebe099)?;
    let start = get_start(cx, o);
    let duration = get_num(cx, o, "duration", 1.4)?.max(0.1);
    let dots = get_num(cx, o, "dots", 13.0)?.clamp(2.0, 64.0) as usize;
    let font = get_face(cx, o, "font", "JetBrains Mono")?;
    let show_label = obj::opt(cx.vm, o, "label").and_then(|v| v.as_bool()) != Some(false);
    let label_text = get_text(cx, o, "label").unwrap_or_else(|| curve.css());

    // The plot and the spacing row, in the box.
    let (px, py, pw, ph) = (w * 0.08, h * 0.14, w * 0.84, h * 0.58);
    let row_y = h * 0.9;
    let unit = h.min(w);
    let line = (unit * 0.004).max(1.0);
    let gx = |x: f64| px + x * pw;
    let gy = |y: f64| py + (1.0 - y) * ph;
    let draw_start = 0.25;
    let t_of = |x: f64| draw_start + x * duration;
    let steps = ((duration * 30.0).ceil() as usize).max(8);
    // The samples of `f` over the draw, as keys.
    let sampled = |cx: &mut Cx, f: &dyn Fn(f64) -> f64| -> ScriptValue {
        let points: Vec<(f64, Lit)> = (0..=steps).map(|k| {
            let x = k as f64 / steps as f64;
            (t_of(x), f(x).into())
        }).collect();
        keys(cx, start, points)
    };

    let mut layers = Vec::new();
    // The value grid: quarters, faint; then the axes drawing on.
    let mut grid = String::new();
    for i in 1..4 {
        let k = i as f64 / 4.0;
        grid.push_str(&format!("M {:.1} {:.1} L {:.1} {:.1} ", gx(k), gy(0.0), gx(k), gy(1.0)));
        grid.push_str(&format!("M {:.1} {:.1} L {:.1} {:.1} ", gx(0.0), gy(k), gx(1.0), gy(k)));
    }
    grid.push_str(&format!("M {:.1} {:.1} L {:.1} {:.1} L {:.1} {:.1}", gx(0.0), gy(1.0), gx(1.0), gy(1.0), gx(1.0), gy(0.0)));
    let grid_fade = tween(cx, 0.0.into(), 1.0.into(), start, 0.3, "ease_out");
    layers.push(layer(cx, "Path", vec![
        ("name", "grid".into()),
        ("svg", grid.into()),
        ("stroke", Lit::Color(with_alpha(ink, crate::load::unpack(ink)[3] * 0.35))),
        ("width", line.into()),
        ("opacity", grid_fade.into()),
    ]));
    let axes_trim = tween(cx, 0.0.into(), 1.0.into(), start, 0.35, "ease_out_cubic");
    layers.push(layer(cx, "Path", vec![
        ("name", "axes".into()),
        ("svg", format!("M {:.1} {:.1} L {:.1} {:.1} L {:.1} {:.1}", gx(0.0), gy(1.0) - ph * 0.04, gx(0.0), gy(0.0), gx(1.0) + pw * 0.03, gy(0.0)).into()),
        ("stroke", Lit::Color(ink)),
        ("width", (line * 1.5).into()),
        ("cap", Lit::Id("round")),
        ("trim_end", axes_trim.into()),
    ]));
    let small = unit * 0.032;
    for (s, at, align) in [("VALUE", [gx(0.0) - small * 0.6, gy(1.0)], "right"), ("TIME", [gx(1.0), gy(0.0) + small * 1.6], "right")] {
        let l = label(cx, s, small * 0.8, ink, at, align, &font);
        let when = at_time(cx, start, 0.1);
        let fade = tween(cx, 0.0.into(), 1.0.into(), when, 0.3, "ease_out");
        obj::set(cx.vm, l.as_object().unwrap(), "opacity", fade);
        layers.push(l);
    }
    if show_label {
        let l = label(cx, &label_text, small, color, [gx(0.0), 0.0], "left", &font);
        let when = at_time(cx, start, 0.15);
        let fade = tween(cx, 0.0.into(), 1.0.into(), when, 0.3, "ease_out");
        obj::set(cx.vm, l.as_object().unwrap(), "opacity", fade);
        layers.push(l);
    }

    // The handles: thin lines out to diamonds, popping in as the curve starts.
    let [x1, y1, x2, y2] = curve.points;
    let d = unit * 0.028;
    for (i, (ax, ay, hx, hy)) in [(0.0, 0.0, x1, y1), (1.0, 1.0, x2, y2)].into_iter().enumerate() {
        let pop = at_time(cx, start, draw_start + 0.05 + i as f64 * 0.12);
        let line_trim = tween(cx, 0.0.into(), 1.0.into(), pop, 0.25, "ease_out_cubic");
        layers.push(layer(cx, "Path", vec![
            ("name", format!("handle line {}", i + 1).into()),
            ("svg", format!("M {:.1} {:.1} L {:.1} {:.1}", gx(ax), gy(ay), gx(hx), gy(hy)).into()),
            ("stroke", Lit::Color(with_alpha(color, 0.6))),
            ("width", line.into()),
            ("trim_end", line_trim.into()),
        ]));
        let pop_at = at_time(cx, pop, 0.15);
        let pop_scale = tween(cx, 0.0.into(), 1.0.into(), pop_at, 0.35, "ease_out_back");
        layers.push(layer(cx, "Rect", vec![
            ("name", format!("handle {}", i + 1).into()),
            ("size", v2(d, d)),
            ("at", v2(gx(hx) - d * 0.5, gy(hy) - d * 0.5)),
            ("rotate", 45.0.into()),
            ("fill", Lit::Color(color)),
            ("scale", pop_scale.into()),
        ]));
    }

    // The curve, drawn on in step with the playhead.
    let [a, b, c, e] = [curve.point(0.0), curve.point(1.0), [x1, y1], [x2, y2]];
    let table = trim_table(&curve, [pw, ph], 400);
    let trim = sampled(cx, &|x| trim_at(&table, x));
    layers.push(layer(cx, "Path", vec![
        ("name", "curve".into()),
        ("svg", format!(
            "M {:.2} {:.2} C {:.2} {:.2}, {:.2} {:.2}, {:.2} {:.2}",
            gx(a[0]), gy(a[1]), gx(c[0]), gy(c[1]), gx(e[0]), gy(e[1]), gx(b[0]), gy(b[1])
        ).into()),
        ("stroke", Lit::Color(color)),
        ("width", (line * 3.0).into()),
        ("cap", Lit::Id("round")),
        ("trim_end", trim.into()),
    ]));

    // The playhead: a line, the point it reads on the curve, and the value.
    let sweep_start = at_time(cx, start, draw_start);
    let sweep = |cx: &mut Cx, from: f64, to: f64| tween(cx, from.into(), to.into(), sweep_start, duration, "linear");
    let head_start = at_time(cx, start, draw_start - 0.1);
    let head_in = tween(cx, 0.0.into(), 1.0.into(), head_start, 0.15, "linear");
    let head_x = sweep(cx, 0.0, pw);
    layers.push(layer(cx, "Rect", vec![
        ("name", "playhead".into()),
        ("size", v2(line * 1.5, ph * 1.08)),
        ("at", v2(gx(0.0) - line * 0.75, gy(1.0) - ph * 0.04)),
        ("x", head_x.into()),
        ("fill", Lit::Color(accent)),
        ("opacity", head_in.into()),
    ]));
    let r = unit * 0.014;
    let reading_x = sweep(cx, 0.0, pw);
    let reading_y = sampled(cx, &|x| -curve.value(x) * ph);
    layers.push(layer(cx, "Ellipse", vec![
        ("name", "reading".into()),
        ("size", v2(r * 2.0, r * 2.0)),
        ("at", v2(gx(0.0) - r, gy(0.0) - r)),
        ("x", reading_x.into()),
        ("y", reading_y.into()),
        ("fill", Lit::Color(accent)),
        ("opacity", head_in.into()),
    ]));
    let number = sampled(cx, &|x| curve.value(x));
    let readout_x = sweep(cx, 0.0, pw);
    layers.push(layer(cx, "Text", vec![
        ("name", "readout".into()),
        ("number", number.into()),
        ("decimals", 2.0.into()),
        ("tabular", true.into()),
        ("font", obj::Lit::Font(font.clone())),
        ("size", small.into()),
        ("color", Lit::Color(accent)),
        ("align", Lit::Id("center")),
        ("at", v2(gx(0.0), gy(1.0) - ph * 0.08)),
        ("x", readout_x.into()),
        ("opacity", head_in.into()),
    ]));

    // The spacing row: ghost circles at the ease's samples, and the ball.
    let br = unit * 0.022;
    for i in 0..dots {
        let x = curve.value(i as f64 / (dots - 1) as f64);
        let when = at_time(cx, start, 0.1 + i as f64 * 0.025);
        let pop = tween(cx, 0.0.into(), 1.0.into(), when, 0.3, "ease_out_back");
        layers.push(layer(cx, "Ellipse", vec![
            ("name", format!("spacing {i}").into()),
            ("size", v2(br * 2.0, br * 2.0)),
            ("at", v2(gx(x) - br, row_y - br)),
            ("stroke", Lit::Color(with_alpha(color, 0.45))),
            ("width", line.into()),
            ("scale", pop.into()),
        ]));
    }
    let ball_x = sampled(cx, &|x| curve.value(x) * pw);
    let mut ball: Vec<(&str, Lit)> = vec![
        ("name", "ball".into()),
        ("size", v2(br * 2.0, br * 2.0)),
        ("at", v2(gx(0.0) - br, row_y - br)),
        ("x", ball_x.into()),
        ("fill", Lit::Color(accent)),
    ];
    let ball_scale = if let Some(fill_at) = obj::opt(cx.vm, o, "fill_at").filter(|v| !v.is_nil()) {
        // Grows about its centre until it covers the whole frame from
        // anywhere in the box.
        let cover = 2.2 * (cx.doc[0].hypot(cx.doc[1])) / (br * 2.0);
        let t0 = at_time(cx, start, 0.1);
        let t1 = at_time(cx, start, 0.4);
        let t3 = at_time(cx, fill_at, 0.45);
        obj::keys(cx.vm, vec![
            (t0, 0.0.into(), None),
            (t1, 1.0.into(), Some("ease_out_back")),
            (fill_at, 1.0.into(), None),
            (t3, cover.into(), Some("ease_in_cubic")),
        ])
    } else {
        let when = at_time(cx, start, 0.1);
        tween(cx, 0.0.into(), 1.0.into(), when, 0.3, "ease_out_back")
    };
    ball.push(("scale", ball_scale.into()));
    layers.push(layer(cx, "Ellipse", ball));
    Ok(group(cx, o, layers, vec![("size", v2(w, h))]))
}
